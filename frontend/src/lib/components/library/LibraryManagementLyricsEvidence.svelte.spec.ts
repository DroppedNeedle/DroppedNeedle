import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { LibraryManagementPlanItem } from '$lib/queries/library-management/types';
import LibraryManagementLyricsEvidence from './LibraryManagementLyricsEvidence.svelte';

const item: LibraryManagementPlanItem = {
	ordinal: 0,
	bundle_ordinal: 0,
	local_album_id: 'album-1',
	local_track_id: 'track-1',
	source_root_id: 'root-1',
	source_relative_path: 'Artist/Album/01 Track.flac',
	destination_root_id: 'root-1',
	destination_relative_path: 'Artist/Album/01 Track.flac',
	eligibility: 'eligible',
	reason_code: null,
	estimated_temporary_bytes: 0,
	desired_document: {},
	artwork_choices: [],
	diff: {
		field_mutations: [
			{
				name: 'lyrics_plain',
				operation: 'set',
				before: null,
				after: 'Pinned lyrics',
				representation_loss: null
			}
		],
		lyrics_projection: {
			status: 'available',
			provider_id: 1,
			provider_revision: 'lyrics-1',
			reason: null,
			plain_available: true,
			synced_available: false,
			plain_selected: true,
			synced_selected: false,
			synced_supported: true,
			preserve_existing: false
		}
	},
	capability: { audio_format: 'flac' },
	collisions: []
};

describe('LibraryManagementLyricsEvidence', () => {
	it('does not promise a future write after an operation stops', async () => {
		await render(LibraryManagementLyricsEvidence, {
			item,
			workState: 'pending',
			operationState: 'stopped'
		});

		await expect.element(page.getByText('Not written', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Will be written')).not.toBeInTheDocument();
	});

	it('treats unscheduled work as terminal', async () => {
		await render(LibraryManagementLyricsEvidence, {
			item,
			workState: 'not_scheduled',
			operationState: 'succeeded'
		});

		await expect.element(page.getByText('Not written', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Will be written')).not.toBeInTheDocument();
	});
});
