import { Channel, invoke, Resource } from '@tauri-apps/api/core';

export type DownloadMode = 'differential' | 'full';

export interface DownloadSummary {
  mode: DownloadMode;
  totalBytes: number;
  downloadedBytes: number;
  reusedBytes: number;
  httpRequests: number;
  savedPercent: number;
  fallbackReason?: string | null;
}

export type DownloadEvent =
  | {
      event: 'Started';
      data: {
        contentLength?: number | null;
        totalSize?: number | null;
        differential: boolean;
        reusedBytes: number;
      };
    }
  | {
      event: 'Progress';
      data: {
        chunkLength: number;
      };
    }
  | {
      event: 'Finished';
      data: {
        summary: DownloadSummary;
      };
    };

export interface CheckOptions {
  headers?: Array<[string, string]>;
  timeout?: number;
  proxy?: string;
  target?: string;
}

export interface DownloadOptions {
  headers?: Array<[string, string]>;
  timeout?: number;
  restartAfterInstall?: boolean;
}

interface UpdateMetadata {
  rid: number;
  currentVersion: string;
  version: string;
  date?: string | null;
  body?: string | null;
  rawJson: Record<string, unknown>;
}

interface DownloadResult {
  rid: number;
  summary: DownloadSummary;
}

class PackageHandle extends Resource {}

export class Update extends Resource {
  readonly currentVersion: string;
  readonly version: string;
  readonly date?: string;
  readonly body?: string;
  readonly rawJson: Record<string, unknown>;
  private downloadedPackage?: PackageHandle;

  constructor(metadata: UpdateMetadata) {
    super(metadata.rid);
    this.currentVersion = metadata.currentVersion;
    this.version = metadata.version;
    this.date = metadata.date ?? undefined;
    this.body = metadata.body ?? undefined;
    this.rawJson = metadata.rawJson;
  }

  async download(
    onEvent?: (event: DownloadEvent) => void,
    options?: DownloadOptions,
  ): Promise<DownloadSummary> {
    if (this.downloadedPackage !== undefined) {
      await this.downloadedPackage.close().catch(() => {});
      this.downloadedPackage = undefined;
    }
    const channel = new Channel<DownloadEvent>();
    if (onEvent) {
      channel.onmessage = onEvent;
    }
    const result = await invoke<DownloadResult>(
      'plugin:updater-plus|download',
      {
        onEvent: channel,
        options,
        rid: this.rid,
      },
    );
    this.downloadedPackage = new PackageHandle(result.rid);
    return result.summary;
  }

  async install(restartAfterInstall?: boolean): Promise<void> {
    if (this.downloadedPackage === undefined) {
      throw new Error('Update package must be downloaded before installing');
    }
    await invoke<void>('plugin:updater-plus|install', {
      options:
        restartAfterInstall === undefined ? undefined : { restartAfterInstall },
      packageRid: this.downloadedPackage.rid,
      updateRid: this.rid,
    });
    this.downloadedPackage = undefined;
  }

  override async close(): Promise<void> {
    if (this.downloadedPackage !== undefined) {
      await this.downloadedPackage.close().catch(() => {});
      this.downloadedPackage = undefined;
    }
    await super.close();
  }

  async downloadAndInstall(
    onEvent?: (event: DownloadEvent) => void,
    options?: DownloadOptions,
  ): Promise<DownloadSummary> {
    const channel = new Channel<DownloadEvent>();
    if (onEvent) {
      channel.onmessage = onEvent;
    }
    return await invoke<DownloadSummary>(
      'plugin:updater-plus|download_and_install',
      {
        onEvent: channel,
        options,
        rid: this.rid,
      },
    );
  }
}

export async function check(options?: CheckOptions): Promise<Update | null> {
  const metadata = await invoke<UpdateMetadata | null>(
    'plugin:updater-plus|check',
    { options },
  );
  return metadata ? new Update(metadata) : null;
}
