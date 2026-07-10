// Hand-mirrors backend/api/v1/schemas/drop_import.py (snake_case wire format).

export type DropImportJobStatus = 'processing' | 'completed' | 'failed';

export type DropImportItemStatus =
	| 'processing'
	| 'imported'
	| 'skipped'
	| 'needs_review'
	| 'failed'
	| 'discarded';

export interface DropImportItem {
	id: number;
	folder_name: string;
	status: DropImportItemStatus;
	updated_at: number;
	release_group_mbid: string | null;
	album_title: string | null;
	artist_name: string | null;
	files_total: number;
	files_imported: number;
	detail: string | null;
}

export interface DropImportJob {
	id: string;
	status: DropImportJobStatus;
	created_at: number;
	upload_name: string;
	user_id: string;
	user_name: string;
	error: string | null;
	items: DropImportItem[];
}

export interface DropImportJobList {
	jobs: DropImportJob[];
}
