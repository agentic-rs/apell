export interface Release {
  version: string;
  sha256: string;
  size_bytes: number;
  published_at: string;
  release_notes: string;
  api_version: number;
  permissions: string[];
  network: boolean;
  has_backend: boolean;
  download_url: string;
  prerelease: boolean;
}

export interface Applet {
  applet_id: string;
  name: string;
  description: string;
  project_url: string | null;
  icon_url: string | null;
  latest_release: Release;
  latest_stable_release: Release | null;
}

export interface AppletDetails {
  applet: Applet;
  releases: Release[];
}
