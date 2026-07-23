import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { LibraryManagementTagEditorContext } from '$lib/queries/library-management/types';
import type { LibraryFileMeta } from '$lib/types';

const { mockCreate, mockGoto, mockQuery, mockRemember } = vi.hoisted(() => ({
	mockCreate: vi.fn(),
	mockGoto: vi.fn(),
	mockQuery: vi.fn(),
	mockRemember: vi.fn()
}));

vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => mockGoto(...args) }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: true, user: { id: 'admin-1' } }
}));
vi.mock('$lib/queries/library-management/LibraryManagementQueries.svelte', () => ({
	getLibraryManagementTagEditorQuery: (...args: unknown[]) => mockQuery(...args)
}));
vi.mock('$lib/queries/library-management/LibraryManagementMutations.svelte', () => ({
	createLibraryManagementTagEditPreviewMutation: () => ({
		mutateAsync: mockCreate,
		isPending: false
	})
}));
vi.mock('$lib/queries/library-management/LibraryManagementPreviewTokens', () => ({
	rememberLibraryManagementPreviewToken: (...args: unknown[]) => mockRemember(...args)
}));
vi.mock('$lib/utils/uuid', () => ({ createUuid: () => 'edit-key-1' }));

import TagEditor from './TagEditor.svelte';

const track: LibraryFileMeta = {
	id: 'track-1',
	title: 'Airbag',
	album_id: 'album-1',
	album_title: 'OK Computer',
	artist_id: 'artist-1',
	artist_name: 'Radiohead',
	album_artist_id: 'artist-1',
	album_artist_name: 'Radiohead',
	musicbrainz_recording_id: 'rec-1',
	musicbrainz_release_group_id: 'rg-1',
	musicbrainz_artist_id: null,
	musicbrainz_album_artist_id: null,
	disc_number: 1,
	track_number: 1,
	year: 1997,
	genre: 'Rock',
	duration_seconds: 260,
	format: 'flac',
	bit_rate: 900,
	sample_rate: 44100,
	bit_depth: 16,
	channels: 2,
	file_size_bytes: 1048576,
	date_added: 1,
	cover_available: false,
	current_tier: 'lossless',
	below_cutoff: false
};

const context = (acceptedIdentity = true): LibraryManagementTagEditorContext => ({
	local_track_id: 'track-1',
	local_album_id: 'album-1',
	root_id: 'root-1',
	profile_id: 'profile-1',
	profile_name: 'Picard-style Organizer',
	settings_revision: 'settings-1',
	policy_revision: 'policy-1',
	track_revision: 3,
	album_revision: 5,
	accepted_identity: acceptedIdentity,
	identity_reason: acceptedIdentity ? null : 'TRACK_NOT_MAPPED',
	fields: [
		{
			field_name: 'title',
			scope: 'track',
			cardinality: 'string',
			current_value: 'Airbag',
			override_id: null,
			override_mode: null,
			override_row_revision: null
		},
		{
			field_name: 'artist',
			scope: 'track',
			cardinality: 'ordered_strings',
			current_value: ['Radiohead'],
			override_id: null,
			override_mode: null,
			override_row_revision: null
		},
		{
			field_name: 'album',
			scope: 'album',
			cardinality: 'string',
			current_value: 'OK Computer',
			override_id: 'override-album',
			override_mode: 'replace',
			override_row_revision: 2
		},
		{
			field_name: 'genre',
			scope: 'track',
			cardinality: 'ordered_strings',
			current_value: ['Rock', 'Alternative'],
			override_id: null,
			override_mode: null,
			override_row_revision: null
		},
		{
			field_name: 'composer',
			scope: 'track',
			cardinality: 'ordered_strings',
			current_value: [],
			override_id: null,
			override_mode: null,
			override_row_revision: null
		}
	]
});

function renderEditor() {
	return render(TagEditor, { track, open: true });
}

describe('TagEditor.svelte', () => {
	beforeEach(() => {
		mockCreate.mockReset();
		mockCreate.mockResolvedValue({
			job_id: 'preview-1',
			preview_token: 'secret-token',
			created_at: 1,
			expires_at: 2,
			existing: false
		});
		mockGoto.mockReset();
		mockGoto.mockResolvedValue(undefined);
		mockRemember.mockReset();
		mockQuery.mockReset();
		mockQuery.mockReturnValue({ data: context(), isPending: false, isError: false });
	});

	it('defaults to a local override and submits list-valued fields to a durable preview', async () => {
		renderEditor();
		await expect.element(page.getByText('Staged metadata edit')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Save as local override/ }))
			.toHaveAttribute('aria-pressed', 'true');
		await expect
			.element(page.getByRole('button', { name: /Write once/ }))
			.toHaveAttribute('aria-pressed', 'false');

		await page.getByRole('textbox', { name: /Artist/ }).fill('Radiohead\nThom Yorke');
		await page.getByRole('textbox', { name: /Genre/ }).fill('Rock\nArt Rock');
		await page.getByRole('button', { name: 'Preview 2 changes' }).click();

		expect(mockCreate).toHaveBeenCalledWith({
			local_track_id: 'track-1',
			mode: 'save_override',
			expected_settings_revision: 'settings-1',
			expected_policy_revision: 'policy-1',
			fields: [
				{ field_name: 'artist', value: ['Radiohead', 'Thom Yorke'] },
				{ field_name: 'genre', value: ['Rock', 'Art Rock'] }
			],
			idempotency_key: 'edit-key-1'
		});
		expect(mockRemember).toHaveBeenCalledWith('preview-1', 'secret-token');
		expect(mockGoto).toHaveBeenCalledWith('/library/management/previews/preview-1');
	});

	it('makes write-once semantics explicit', async () => {
		renderEditor();
		await page.getByRole('button', { name: /Write once/ }).click();
		await expect
			.element(page.getByRole('button', { name: /Write once/ }))
			.toHaveAttribute('aria-pressed', 'true');
		await page.getByRole('textbox', { name: /Title/ }).fill('Airbag (Live)');
		await page.getByRole('button', { name: 'Preview 1 change' }).click();

		expect(mockCreate).toHaveBeenCalledWith(
			expect.objectContaining({
				mode: 'write_once',
				fields: [{ field_name: 'title', value: 'Airbag (Live)' }]
			})
		);
	});

	it('never preselects override resets and warns when album scope expands', async () => {
		renderEditor();
		await page.getByRole('button', { name: /Reset to canonical/ }).click();
		const albumOverride = page.getByRole('checkbox', { name: /Album/ });
		await expect.element(albumOverride).not.toBeChecked();
		await expect.element(page.getByRole('button', { name: /Preview changes/ })).toBeDisabled();

		await albumOverride.click();
		await expect.element(page.getByText(/apply consistently to every mapped track/)).toBeVisible();
		await page.getByRole('button', { name: 'Preview 1 change' }).click();
		expect(mockCreate).toHaveBeenCalledWith(
			expect.objectContaining({
				mode: 'reset_canonical',
				fields: [{ field_name: 'album' }]
			})
		);
	});

	it('blocks mutation without an accepted release-track mapping', async () => {
		mockQuery.mockReturnValue({ data: context(false), isPending: false, isError: false });
		renderEditor();
		await expect.element(page.getByText(/needs an accepted MusicBrainz release/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Preview changes/ })).toBeDisabled();
	});

	it('shows a query failure instead of an endless loading state', async () => {
		mockQuery.mockReturnValue({ data: undefined, isPending: false, isError: true });
		renderEditor();

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent("Could not load the file's current metadata.");
		await expect.element(page.getByLabelText('Loading tag editor')).not.toBeInTheDocument();
	});
});
