package update

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Automatic checks at most once daily per installed version, including failed
// attempts. Development builds and CP_NO_UPDATE_CHECK=1 never contact GitHub.
// Cached attempts return Available=false so notices are also limited to daily.
func (c *Client) Automatic(ctx context.Context, current, cacheDir string) (Result, error) {
	result := Result{Current: current}
	if os.Getenv("CP_NO_UPDATE_CHECK") == "1" || !stable(current) {
		return result, nil
	}
	if err := os.MkdirAll(cacheDir, 0o700); err != nil {
		return result, err
	}
	lock := filepath.Join(cacheDir, "update.lock")
	if !c.cacheLock(lock) {
		return result, nil
	}
	defer os.Remove(lock)
	path := filepath.Join(cacheDir, "update-"+current+".json")
	var checked time.Time
	if data, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(data, &checked)
	}
	now := c.now()
	if age := now.Sub(checked); age >= 0 && age < 24*time.Hour {
		return result, nil
	}
	// Record before requesting so failures and interrupted requests are throttled.
	data, err := json.Marshal(now)
	if err != nil {
		return result, err
	}
	if err = os.WriteFile(path, data, 0o600); err != nil {
		return result, fmt.Errorf("record update check: %w", err)
	}
	ctx, cancel := context.WithTimeout(ctx, time.Second)
	defer cancel()
	return c.Check(ctx, current)
}

func (c *Client) cacheLock(path string) bool {
	if err := os.Mkdir(path, 0o700); err == nil {
		return true
	} else if !errors.Is(err, os.ErrExist) {
		return false
	}
	info, err := os.Stat(path)
	if err != nil || c.now().Sub(info.ModTime()) < time.Minute {
		return false
	}
	// A check lasts at most one second; a minute-old lock survived its process.
	if err = os.Remove(path); err != nil {
		return false
	}
	return os.Mkdir(path, 0o700) == nil
}
