package update

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"golang.org/x/mod/semver"
)

var binaries = [...]string{"cpd", "cpctl"}

// Install verifies and stages both binaries before replacing them in dir.
// Each replacement is atomic on supported platforms; a running daemon must be
// restarted separately. Reinstalling the same version repairs mismatched pairs.
func (c *Client) Install(ctx context.Context, current, dir string) (Result, error) {
	ctx, cancel := context.WithTimeout(ctx, 2*time.Minute)
	defer cancel()
	result := Result{Current: current}
	if (c.goos != "linux" && c.goos != "darwin") || (c.goarch != "amd64" && c.goarch != "arm64") {
		return result, fmt.Errorf("no release binary for %s/%s", c.goos, c.goarch)
	}
	dir, err := filepath.EvalSymlinks(dir)
	if err != nil {
		return result, err
	}
	lock := filepath.Join(dir, ".clanker-proxy-update.lock")
	if err = os.Mkdir(lock, 0o700); err != nil {
		if errors.Is(err, os.ErrExist) {
			return result, fmt.Errorf("installation is locked at %s; remove this directory only after confirming no installer or updater is running: %w", lock, err)
		}
		return result, fmt.Errorf("lock installation (check directory permissions or another updater): %w", err)
	}
	defer os.Remove(lock)
	stage, err := os.MkdirTemp(dir, ".clanker-proxy-update-")
	if err != nil {
		return result, err
	}
	retainStage := false
	defer func() {
		if !retainStage {
			_ = os.RemoveAll(stage)
		}
	}()
	if err = backupTargets(dir, stage); err != nil {
		return result, err
	}
	result, err = c.Check(ctx, current)
	if err != nil {
		return result, err
	}
	if semver.IsValid(current) && semver.Compare(current, result.Latest) > 0 {
		return result, fmt.Errorf("refusing downgrade from %s to %s", current, result.Latest)
	}
	archive, err := c.archive(ctx, result.Latest)
	if err != nil {
		return result, err
	}
	if err = extract(archive, stage); err != nil {
		return result, fmt.Errorf("unpack release: %w", err)
	}
	if err = ctx.Err(); err != nil {
		return result, err
	}
	if err = replaceTargets(dir, stage); err != nil {
		retainStage = true
		return result, fmt.Errorf("%w; recovery files retained in %s", err, stage)
	}
	result.Installed = true
	return result, nil
}

func (c *Client) archive(ctx context.Context, version string) ([]byte, error) {
	name := "clanker-proxy_" + c.goos + "_" + c.goarch + ".tar.gz"
	base := c.downloadURL + "/" + version + "/"
	checksums, err := c.get(ctx, base+"checksums.txt", 64<<10)
	if err != nil {
		return nil, err
	}
	expected, err := checksum(checksums, name)
	if err != nil {
		return nil, err
	}
	archive, err := c.get(ctx, base+name, 64<<20)
	if err != nil {
		return nil, err
	}
	actual := sha256.Sum256(archive)
	if !bytes.Equal(actual[:], expected) {
		return nil, errors.New("release archive SHA256 checksum mismatch")
	}
	return archive, nil
}

func checksum(manifest []byte, name string) ([]byte, error) {
	var result []byte
	for line := range strings.SplitSeq(string(manifest), "\n") {
		fields := strings.Fields(line)
		if len(fields) != 2 || strings.TrimPrefix(fields[1], "*") != name {
			continue
		}
		if result != nil {
			return nil, errors.New("release checksum manifest contains duplicate archive entries")
		}
		decoded, err := hex.DecodeString(fields[0])
		if err != nil || len(decoded) != sha256.Size {
			return nil, errors.New("release checksum is not a SHA256 digest")
		}
		result = decoded
	}
	if result == nil {
		return nil, fmt.Errorf("release checksum manifest has no entry for %s", name)
	}
	return result, nil
}

func extract(archive []byte, stage string) error {
	gz, err := gzip.NewReader(bytes.NewReader(archive))
	if err != nil {
		return err
	}
	defer gz.Close()
	limited := &io.LimitedReader{R: gz, N: 256 << 20}
	reader := tar.NewReader(limited)
	seen := make(map[string]bool)
	for {
		header, nextErr := reader.Next()
		if errors.Is(nextErr, io.EOF) {
			break
		}
		if nextErr != nil {
			return nextErr
		}
		if err = extractEntry(reader, header, stage, seen); err != nil {
			return err
		}
	}
	if _, err = io.Copy(io.Discard, limited); err != nil {
		return err
	}
	if limited.N == 0 {
		return errors.New("unpacked archive exceeds size limit")
	}
	if !seen["cpd"] || !seen["cpctl"] {
		return errors.New("release archive must contain both cpd and cpctl")
	}
	return nil
}

func extractEntry(reader io.Reader, header *tar.Header, stage string, seen map[string]bool) error {
	name := header.Name
	metadata := name == "LICENSE" || name == "README.md"
	if (name != "cpd" && name != "cpctl" && !metadata) || header.Typeflag != tar.TypeReg || seen[name] {
		return fmt.Errorf("unexpected release archive entry %q", name)
	}
	if header.Size <= 0 || header.Size > 128<<20 {
		return fmt.Errorf("invalid size for %s", name)
	}
	seen[name] = true
	if metadata {
		if header.Size > 1<<20 {
			return errors.New("release metadata exceeds size limit")
		}
		_, err := io.CopyN(io.Discard, reader, header.Size)
		return err
	}
	file, err := os.OpenFile(filepath.Join(stage, name), os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o700) //nolint:gosec // The entry name is restricted to the two exact binary names above.
	if err != nil {
		return err
	}
	defer file.Close()
	if _, err = io.CopyN(file, reader, header.Size); err != nil {
		return err
	}
	if err = file.Chmod(0o755); err != nil {
		return err
	}
	if err = file.Sync(); err != nil {
		return err
	}
	return file.Close()
}

func backupTargets(dir, stage string) error {
	for _, name := range binaries {
		path := filepath.Join(dir, name)
		info, err := os.Lstat(path)
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			return fmt.Errorf("refusing to replace non-regular file %s", path)
		}
		// Hard links preserve the original inode for rollback without moving a
		// binary away from its public path while downloads are in progress.
		if err = os.Link(path, filepath.Join(stage, name+".previous")); err != nil {
			return fmt.Errorf("back up %s: %w", name, err)
		}
	}
	return nil
}

func replaceTargets(dir, stage string) error {
	for i, name := range binaries {
		if err := os.Rename(filepath.Join(stage, name), filepath.Join(dir, name)); err != nil {
			return errors.Join(fmt.Errorf("replace %s: %w", name, err), rollback(dir, stage, binaries[:i]))
		}
	}
	return nil
}

func rollback(dir, stage string, names []string) error {
	var errs []error
	for _, name := range names {
		backup, target := filepath.Join(stage, name+".previous"), filepath.Join(dir, name)
		err := os.Rename(backup, target)
		if errors.Is(err, os.ErrNotExist) {
			err = os.Remove(target)
		}
		if err != nil {
			errs = append(errs, fmt.Errorf("restore %s: %w", name, err))
		}
	}
	return errors.Join(errs...)
}
