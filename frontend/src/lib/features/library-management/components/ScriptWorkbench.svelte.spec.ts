import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { fakeApi, type FakeRequest, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import ScriptWorkbench from './ScriptWorkbench.svelte';

type Preview = RouteData<'POST /api/v3/settings/library/script-preview'>;

const VALUES: Record<string, string> = {
	albumartist: 'Radiohead',
	title: 'Paranoid Android',
	year: '1997',
	ext: 'flac'
};

// A stand-in for the server: each `{name}` becomes its value.
function answer(request: FakeRequest): Preview {
	const { source } = request.body as { source: string };
	const path = source.replace(/\{(\w+)\}/g, (_, name: string) => VALUES[name] ?? '');
	return {
		fault: null,
		samples: (['one_disc', 'two_discs', 'name_shared'] as const).map((sample) => ({
			sample,
			path,
			empty_name_parts: null,
			fault: null,
			tags: [],
			values: VALUES
		}))
	};
}

describe('the script workbench', () => {
	it('shows the example again once typing pauses', async () => {
		const api = fakeApi({ 'POST /api/v3/settings/library/script-preview': answer });
		await renderWithQuery(ScriptWorkbench, {
			kind: 'import_naming',
			label: 'Import naming script',
			value: '{albumartist}/{title}.{ext}'
		});

		await expect.element(page.getByText('Radiohead/Paranoid Android.flac')).toBeVisible();
		await page.getByLabelText('Import naming script').fill('{albumartist}/{year} {title}.{ext}');

		await expect.element(page.getByText('Radiohead/1997 Paranoid Android.flac')).toBeVisible();
		const sent = api.calls('POST /api/v3/settings/library/script-preview');
		expect(sent.at(-1)?.body).toEqual({
			kind: 'import_naming',
			source: '{albumartist}/{year} {title}.{ext}'
		});

		await page.getByLabelText('Import naming script').fill('');
		await expect.element(page.getByText('Write a script to see an example.')).toBeVisible();
		await expect
			.element(page.getByText('Radiohead/1997 Paranoid Android.flac'))
			.not.toBeInTheDocument();
	});

	it('puts a block at the cursor and selects it', async () => {
		fakeApi({ 'POST /api/v3/settings/library/script-preview': answer });
		await renderWithQuery(ScriptWorkbench, {
			kind: 'import_naming',
			label: 'Import naming script',
			value: '{albumartist}/{title}.{ext}'
		});
		await expect.element(page.getByText('Radiohead/Paranoid Android.flac')).toBeVisible();

		const box = page.getByLabelText('Import naming script').element() as HTMLTextAreaElement;
		box.focus();
		box.setSelectionRange('{albumartist}/'.length, '{albumartist}/'.length);
		await page.getByRole('button', { name: /^Year\s*1997$/ }).click();

		await expect
			.element(page.getByLabelText('Import naming script'))
			.toHaveValue('{albumartist}/{year}{title}.{ext}');
		expect(box.value.slice(box.selectionStart, box.selectionEnd)).toBe('{year}');
		await expect.element(page.getByText('Radiohead/1997Paranoid Android.flac')).toBeVisible();

		// Typing ends what Undo can take back, so it never drops typed text.
		await expect.element(page.getByRole('button', { name: 'Undo' })).toBeVisible();
		await page.getByLabelText('Import naming script').fill('{albumartist}/{year} {title}.{ext}');
		await expect.element(page.getByRole('button', { name: 'Undo' })).not.toBeInTheDocument();
	});

	it('puts the cursor in the quotes of a line that needs a value', async () => {
		fakeApi({ 'POST /api/v3/settings/library/script-preview': answer });
		await renderWithQuery(ScriptWorkbench, {
			kind: 'tagging',
			label: 'Tagging script',
			value: 'delete comment'
		});
		const box = page.getByLabelText('Tagging script').element() as HTMLTextAreaElement;
		box.focus();
		box.setSelectionRange(box.value.length, box.value.length);
		await page.getByRole('button', { name: /^Set a tag/ }).click();

		await expect
			.element(page.getByLabelText('Tagging script'))
			.toHaveValue('delete comment\nset comment = ""');
		expect(box.selectionStart).toBe(box.value.length - 1);
		expect(box.selectionEnd).toBe(box.value.length - 1);
	});
});
