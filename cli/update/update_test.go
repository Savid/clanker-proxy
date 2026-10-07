package update

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

type archiveEntry struct {
	name string
	kind byte
}

func releaseArchive(t *testing.T, entries ...archiveEntry) []byte {
	t.Helper()
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	tw := tar.NewWriter(gz)
	for _, entry := range entries {
		body := "new " + entry.name
		header := &tar.Header{Name: entry.name, Typeflag: entry.kind, Mode: 0o755, Size: int64(len(body))}
		if entry.kind != tar.TypeReg {
			header.Size = 0
			header.Linkname = "cpd"
			body = ""
		}
		if err := tw.WriteHeader(header); err != nil {
			t.Fatal(err)
		}
		if _, err := tw.Write([]byte(body)); err != nil {
			t.Fatal(err)
		}
	}
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func releaseClient(t *testing.T, archive []byte, manifest string) (*Client, *atomic.Int32) {
	t.Helper()
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		switch r.URL.Path {
		case "/latest":
			_, _ = fmt.Fprint(w, `{"tag_name":"v1.2.3"}`)
		case "/v1.2.3/checksums.txt":
			_, _ = fmt.Fprint(w, manifest)
		case "/v1.2.3/clanker-proxy_linux_amd64.tar.gz":
			_, _ = w.Write(archive)
		default:
			http.NotFound(w, r)
		}
	}))
	t.Cleanup(server.Close)
	client := NewClient()
	client.apiURL, client.downloadURL = server.URL+"/latest", server.URL
	client.goos, client.goarch = "linux", "amd64"
	return client, &calls
}

func manifestFor(archive []byte) string {
	return fmt.Sprintf("%x  clanker-proxy_linux_amd64.tar.gz\n", sha256.Sum256(archive))
}

func writeFile(t *testing.T, path, body string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}

func assertFile(t *testing.T, path, want string) {
	t.Helper()
	got, err := os.ReadFile(path)
	if err != nil || string(got) != want {
		t.Fatalf("read %s = %q, %v; want %q", path, got, err, want)
	}
}

func TestInstallBothAndReinstall(t *testing.T) {
	t.Parallel()
	archive := releaseArchive(t, archiveEntry{"cpd", tar.TypeReg}, archiveEntry{"cpctl", tar.TypeReg}, archiveEntry{"LICENSE", tar.TypeReg}, archiveEntry{"README.md", tar.TypeReg})
	client, _ := releaseClient(t, archive, manifestFor(archive))
	for _, current := range []string{"v1.0.0", "v1.2.3", "dev"} {
		t.Run(current, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			for _, name := range binaries {
				writeFile(t, filepath.Join(dir, name), "old "+name)
			}
			result, err := client.Install(t.Context(), current, dir)
			if err != nil || !result.Installed || result.Latest != "v1.2.3" {
				t.Fatalf("Install = %+v, %v", result, err)
			}
			for _, name := range binaries {
				path := filepath.Join(dir, name)
				assertFile(t, path, "new "+name)
				info, statErr := os.Stat(path)
				if statErr != nil || info.Mode().Perm() != 0o755 {
					t.Fatalf("mode %s: %v, %v", name, info, statErr)
				}
			}
		})
	}
}

func TestInvalidReleasePreservesBoth(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name        string
		entries     []archiveEntry
		badChecksum bool
	}{
		{"checksum", []archiveEntry{{"cpd", tar.TypeReg}, {"cpctl", tar.TypeReg}}, true},
		{"missing", []archiveEntry{{"cpd", tar.TypeReg}}, false},
		{"traversal", []archiveEntry{{"../cpd", tar.TypeReg}, {"cpctl", tar.TypeReg}}, false},
		{"symlink", []archiveEntry{{"cpd", tar.TypeReg}, {"cpctl", tar.TypeSymlink}}, false},
		{"duplicate", []archiveEntry{{"cpd", tar.TypeReg}, {"cpd", tar.TypeReg}, {"cpctl", tar.TypeReg}}, false},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			archive := releaseArchive(t, test.entries...)
			manifest := manifestFor(archive)
			if test.badChecksum {
				manifest = manifestFor([]byte("other"))
			}
			client, _ := releaseClient(t, archive, manifest)
			dir := t.TempDir()
			for _, name := range binaries {
				writeFile(t, filepath.Join(dir, name), "old "+name)
			}
			if _, err := client.Install(t.Context(), "v1.0.0", dir); err == nil {
				t.Fatal("invalid release installed")
			}
			for _, name := range binaries {
				assertFile(t, filepath.Join(dir, name), "old "+name)
			}
		})
	}
}

func TestInstallRefusesTargetSymlink(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	other := filepath.Join(t.TempDir(), "other")
	writeFile(t, other, "unrelated")
	writeFile(t, filepath.Join(dir, "cpd"), "old cpd")
	if err := os.Symlink(other, filepath.Join(dir, "cpctl")); err != nil {
		t.Fatal(err)
	}
	client, calls := releaseClient(t, nil, "")
	if _, err := client.Install(t.Context(), "v1.0.0", dir); err == nil {
		t.Fatal("symlink target accepted")
	}
	if calls.Load() != 0 {
		t.Fatal("downloaded before validating destination")
	}
	assertFile(t, other, "unrelated")
	assertFile(t, filepath.Join(dir, "cpd"), "old cpd")
}

func TestReplacementFailureRollsBack(t *testing.T) {
	t.Parallel()
	dir, stage := t.TempDir(), t.TempDir()
	for _, name := range binaries {
		writeFile(t, filepath.Join(dir, name), "old "+name)
	}
	if err := backupTargets(dir, stage); err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(stage, "cpd"), "new cpd")
	if err := replaceTargets(dir, stage); err == nil {
		t.Fatal("missing second staged binary succeeded")
	}
	for _, name := range binaries {
		assertFile(t, filepath.Join(dir, name), "old "+name)
	}
}

func TestAutomaticThrottlesSuccessAndFailure(t *testing.T) {
	t.Setenv("CP_NO_UPDATE_CHECK", "")
	for _, failure := range []bool{false, true} {
		t.Run(strconv.FormatBool(failure), func(t *testing.T) {
			client, calls := releaseClient(t, nil, "")
			if failure {
				client.apiURL += "/missing"
			}
			now := time.Now()
			client.now = func() time.Time { return now }
			dir := t.TempDir()
			first, err := client.Automatic(t.Context(), "v1.0.0", dir)
			if (err != nil) != failure || first.Available == failure {
				t.Fatalf("first = %+v, %v", first, err)
			}
			second, err := client.Automatic(t.Context(), "v1.0.0", dir)
			if err != nil || second.Available || calls.Load() != 1 {
				t.Fatalf("cached = %+v, %v; calls=%d", second, err, calls.Load())
			}
			now = now.Add(24 * time.Hour)
			_, _ = client.Automatic(t.Context(), "v1.0.0", dir)
			if calls.Load() != 2 {
				t.Fatalf("next day calls=%d", calls.Load())
			}
		})
	}
}

func TestAutomaticOptOutAndDevelopment(t *testing.T) {
	client, calls := releaseClient(t, nil, "")
	t.Setenv("CP_NO_UPDATE_CHECK", "1")
	_, _ = client.Automatic(t.Context(), "v1.0.0", t.TempDir())
	t.Setenv("CP_NO_UPDATE_CHECK", "")
	for _, version := range []string{"dev", "0123456", "v1.0.0-dirty", "v1.1.0-rc.1"} {
		_, _ = client.Automatic(t.Context(), version, t.TempDir())
	}
	if calls.Load() != 0 {
		t.Fatalf("disabled checks contacted server %d times", calls.Load())
	}
}

func TestCheckVersionAndCancellation(t *testing.T) {
	t.Parallel()
	client, _ := releaseClient(t, nil, "")
	result, err := client.Check(t.Context(), "v2.0.0")
	if err != nil || result.Available {
		t.Fatalf("newer version = %+v, %v", result, err)
	}
	if _, err = client.Install(t.Context(), "v2.0.0", t.TempDir()); err == nil || !strings.Contains(err.Error(), "downgrade") {
		t.Fatalf("downgrade = %v", err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err = client.Check(ctx, "v1.0.0"); err == nil {
		t.Fatal("cancellation ignored")
	}
}

func TestRejectInvalidChecksums(t *testing.T) {
	t.Parallel()
	valid := manifestFor([]byte("archive"))
	for _, manifest := range []string{"", "bad  clanker-proxy_linux_amd64.tar.gz", valid + valid} {
		if _, err := checksum([]byte(manifest), "clanker-proxy_linux_amd64.tar.gz"); err == nil {
			t.Fatalf("accepted invalid manifest %q", manifest)
		}
	}
}

func TestStableTags(t *testing.T) {
	t.Parallel()
	for _, tag := range []string{"v1", "v1.2", "v1.2.3-rc.1", "v1.2.3+build", "v01.2.3", "../../bin", "1.2.3"} {
		if stable(tag) {
			t.Errorf("accepted tag %q", tag)
		}
	}
	if !stable("v0.1.0") {
		t.Fatal("rejected stable tag")
	}
}

func TestHTTPResponseBoundAndRedirect(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/redirect" {
			http.Redirect(w, r, "/data", http.StatusFound)
			return
		}
		_, _ = fmt.Fprint(w, "oversized response")
	}))
	t.Cleanup(server.Close)
	client := NewClient()
	if _, err := client.get(t.Context(), server.URL+"/data", 4); err == nil {
		t.Fatal("response size limit ignored")
	}
	if _, err := client.get(t.Context(), server.URL+"/redirect", 100); err == nil {
		t.Fatal("insecure redirect allowed")
	}
}
