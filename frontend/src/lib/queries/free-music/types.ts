// Hand-mirrors backend/api/v1/schemas/free_music.py (snake_case wire format).

export type FreeMusicStatus =
	| 'searching'
	| 'downloading'
	| 'importing'
	| 'completed'
	| 'failed'
	| 'cancelled';

export interface FreeMusicTask {
	id: string;
	user_id: string;
	kind: 'album' | 'track';
	mbid: string;
	artist: string;
	title: string;
	status: FreeMusicStatus;
	created_at: number;
	updated_at: number;
	identifier: string;
	licence_url: string;
	format: string;
	files_total: number;
	files_completed: number;
	bytes_total: number;
	bytes_downloaded: number;
	error: string | null;
	quality_snapshot_hash: string | null;
	quality_snapshot_summary: string | null;
}

export interface FreeMusicTasks {
	tasks: FreeMusicTask[];
}
