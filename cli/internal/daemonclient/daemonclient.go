// Package daemonclient is a wrapper around a grpc client
// to talk to turbod
package daemonclient

import (
	"context"
	"path/filepath"

	"github.com/vercel/turbo/cli/internal/daemon/connector"
	"github.com/vercel/turbo/cli/internal/fs"
	"github.com/vercel/turbo/cli/internal/turbodprotocol"
	"github.com/vercel/turbo/cli/internal/turbopath"
)

// DaemonClient provides access to higher-level functionality from the daemon to a turbo run.
type DaemonClient struct {
	client *connector.Client
}

// Status provides details about the daemon's status
type Status struct {
	UptimeMs uint64                       `json:"uptimeMs"`
	LogFile  turbopath.AbsoluteSystemPath `json:"logFile"`
	PidFile  turbopath.AbsoluteSystemPath `json:"pidFile"`
	SockFile turbopath.AbsoluteSystemPath `json:"sockFile"`
}

// New creates a new instance of a DaemonClient.
func New(client *connector.Client) *DaemonClient {
	return &DaemonClient{
		client: client,
	}
}

// GetChangedOutputs implements runcache.OutputWatcher.GetChangedOutputs
func (d *DaemonClient) GetChangedOutputs(ctx context.Context, hash string, repoRelativeOutputGlobs []string) ([]string, int, error) {
	// The daemon expects globs to be unix paths
	var outputGlobs []string
	for _, outputGlob := range repoRelativeOutputGlobs {
		outputGlobs = append(outputGlobs, filepath.ToSlash(outputGlob))
	}
	resp, err := d.client.GetChangedOutputs(ctx, &turbodprotocol.GetChangedOutputsRequest{
		Hash:        hash,
		OutputGlobs: outputGlobs,
	})
	if err != nil {
		return nil, 0, err
	}
	return resp.ChangedOutputGlobs, int(resp.TimeSaved), nil
}

// NotifyOutputsWritten implements runcache.OutputWatcher.NotifyOutputsWritten
func (d *DaemonClient) NotifyOutputsWritten(ctx context.Context, hash string, repoRelativeOutputGlobs fs.TaskOutputs, timeSaved int) error {
	// The daemon expects globs to be unix paths
	var inclusions []string
	var exclusions []string
	for _, inclusion := range repoRelativeOutputGlobs.Inclusions {
		inclusions = append(inclusions, filepath.ToSlash(inclusion))
	}
	for _, exclusion := range repoRelativeOutputGlobs.Exclusions {
		exclusions = append(exclusions, filepath.ToSlash(exclusion))
	}
	_, err := d.client.NotifyOutputsWritten(ctx, &turbodprotocol.NotifyOutputsWrittenRequest{
		Hash:                 hash,
		OutputGlobs:          inclusions,
		OutputExclusionGlobs: exclusions,
		TimeSaved:            uint64(timeSaved),
	})
	return err
}

// Status returns the DaemonStatus from the daemon
func (d *DaemonClient) Status(ctx context.Context) (*Status, error) {
	resp, err := d.client.Status(ctx, &turbodprotocol.StatusRequest{})
	if err != nil {
		return nil, err
	}
	daemonStatus := resp.DaemonStatus
	return &Status{
		UptimeMs: daemonStatus.UptimeMsec,
		LogFile:  d.client.LogPath,
		PidFile:  d.client.PidPath,
		SockFile: d.client.SockPath,
	}, nil
}
