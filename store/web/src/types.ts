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

export interface User {
  github_id: number;
  github_login: string;
}

export interface Session {
  login_available: boolean;
  user: User | null;
  csrf_token: string | null;
}

export interface ManagedPackage {
  applet_id: string;
  role: 'owner' | 'maintainer';
  published: boolean;
}

export interface TrustedPublisher {
  applet_id: string;
  repository?: string | null;
  repository_id: number;
  repository_owner_id: number;
  workflow: string;
  allowed_refs: string[];
  event_names: string[];
  environment: string | null;
}

export interface Invitation {
  id: string;
  applet_id: string;
  kind: 'maintainer' | 'ownership';
  target: User;
  invited_by: User;
  expires_at: number;
}

export interface AccountPackages {
  packages: ManagedPackage[];
  invitations: Invitation[];
}

export interface PackageDetails {
  applet_id: string;
  role: 'owner' | 'maintainer';
  owner: User;
  maintainers: User[];
  publisher: TrustedPublisher | null;
  invitations: Invitation[];
}

export interface PublishResult {
  applet_id: string;
  version: string;
  sha256: string;
  created: boolean;
}
