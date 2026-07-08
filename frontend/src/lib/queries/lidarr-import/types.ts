// Hand-mirrors backend api/v1/schemas/lidarr_import.py + the
// LidarrImportConnectionSettings in schemas/settings.py (no codegen).

export interface LidarrImportConnection {
	url: string;
	api_key: string;
}

export interface LidarrTestResult {
	valid: boolean;
	version?: string | null;
	message: string;
}

export interface LidarrImportStatus {
	configured: boolean;
}

export interface LidarrArtistCandidate {
	mbid: string;
	name: string;
	monitor_new_items: string; // "none" | "all"
	already_following: boolean;
	would_auto_download: boolean;
}

export interface LidarrArtistList {
	artists: LidarrArtistCandidate[];
	total: number;
}

export interface LidarrImportResult {
	imported: number;
	already_following: number;
	skipped_invalid: number;
	auto_download_enabled: number;
	approval_batch_id?: string | null;
}
