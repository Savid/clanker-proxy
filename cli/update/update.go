// Package update checks and installs published clanker-proxy releases.
package update

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"time"

	"golang.org/x/mod/semver"
)

const repository = "savid/clanker-proxy"

// Result describes the latest stable release and whether it was installed.
type Result struct {
	Current   string `json:"current"`
	Latest    string `json:"latest"`
	Available bool   `json:"available"`
	Installed bool   `json:"installed"`
}

// Client retrieves release metadata and checksum-verified binaries from GitHub.
type Client struct {
	http        *http.Client
	apiURL      string
	downloadURL string
	goos        string
	goarch      string
	now         func() time.Time
}

// NewClient returns a client for the official release repository.
func NewClient() *Client {
	return &Client{
		http:        &http.Client{Timeout: 2 * time.Minute, CheckRedirect: secureRedirect},
		apiURL:      "https://api.github.com/repos/" + repository + "/releases/latest",
		downloadURL: "https://github.com/" + repository + "/releases/download",
		goos:        runtime.GOOS, goarch: runtime.GOARCH, now: time.Now,
	}
}

// Check compares current with GitHub's latest stable release.
func (c *Client) Check(ctx context.Context, current string) (Result, error) {
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	result := Result{Current: current}
	body, err := c.get(ctx, c.apiURL, 1<<20)
	if err != nil {
		return result, err
	}
	var release struct {
		// GitHub's API uses snake_case names.
		Tag        string `json:"tag_name"` //nolint:tagliatelle // GitHub wire format.
		Draft      bool   `json:"draft"`
		Prerelease bool   `json:"prerelease"`
	}
	if err = json.Unmarshal(body, &release); err != nil {
		return result, fmt.Errorf("decode release: %w", err)
	}
	if release.Draft || release.Prerelease || !stable(release.Tag) {
		return result, errors.New("GitHub latest release is not a stable vMAJOR.MINOR.PATCH tag")
	}
	result.Latest = release.Tag
	result.Available = !semver.IsValid(current) || semver.Compare(current, result.Latest) < 0
	return result, nil
}

func stable(version string) bool {
	return semver.IsValid(version) && semver.Canonical(version) == version && semver.Prerelease(version) == ""
}

func (c *Client) get(ctx context.Context, url string, limit int64) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", "clanker-proxy-updater")
	req.Header.Set("Accept", "application/vnd.github+json")
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("fetch release: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetch release: HTTP %d", resp.StatusCode)
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, limit+1))
	if err != nil {
		return nil, fmt.Errorf("read release: %w", err)
	}
	if int64(len(body)) > limit {
		return nil, errors.New("release response exceeds size limit")
	}
	return body, nil
}

// ExecutableDir resolves launch symlinks to the directory containing the binary.
func ExecutableDir() (string, error) {
	executable, err := os.Executable()
	if err != nil {
		return "", err
	}
	resolved, err := filepath.EvalSymlinks(executable)
	if err != nil {
		return "", err
	}
	return filepath.Dir(resolved), nil
}

// CacheDir returns the shared per-user directory for automatic check timestamps.
func CacheDir() (string, error) {
	dir, err := os.UserCacheDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "clanker-proxy"), nil
}

func secureRedirect(req *http.Request, via []*http.Request) error {
	if req.URL.Scheme != "https" {
		return errors.New("refusing insecure release redirect")
	}
	if len(via) >= 5 {
		return errors.New("too many release redirects")
	}
	return nil
}
