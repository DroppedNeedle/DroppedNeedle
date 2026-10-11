import type { components } from '$lib/api/v3/openapi';

// The generated v3 drop-import schemas, with the status values the server
// sends spelled out.

export type DropImportJobStatus = 'processing' | 'completed' | 'failed';

export type DropImportItemStatus =
	'processing' | 'imported' | 'skipped' | 'needs_review' | 'failed' | 'discarded';

export type DropImportItem = Omit<components['schemas']['DropImportItemResponse'], 'status'> & {
	status: DropImportItemStatus;
};

export type DropImportJob = Omit<
	components['schemas']['DropImportJobResponse'],
	'status' | 'items'
> & {
	status: DropImportJobStatus;
	items: DropImportItem[];
};

export interface DropImportJobList {
	jobs: DropImportJob[];
}
