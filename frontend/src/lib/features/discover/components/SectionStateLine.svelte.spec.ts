import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { SectionState } from '../queries/types';
import SectionStateLine from './SectionStateLine.svelte';

const KEY = 'radio_sections';
const ZONE = 'made_for_you';

const cases: [SectionState, RegExp][] = [
	[
		{ key: KEY, zone: ZONE, state: 'kept', source: 'listenbrainz', since: 1_763_000_000 },
		/^ListenBrainz isn't answering\. Showing these from .+\.$/
	],
	[
		{ key: KEY, zone: ZONE, state: 'source_down', source: 'listenbrainz' },
		/^ListenBrainz isn't answering\. This fills in when it's back\.$/
	],
	[
		{ key: KEY, zone: ZONE, state: 'needs_link', source: 'lastfm' },
		/^Connect Last\.fm to fill this\.$/
	],
	[{ key: KEY, zone: ZONE, state: 'thin' }, /^That's all for now\.$/],
	[
		{ key: KEY, zone: ZONE, state: 'empty' },
		/^Nothing here yet\. Play, heart or follow a few artists\.$/
	],
	[{ key: KEY, zone: ZONE, state: 'building' }, /^Still building\.$/]
];

function show(status: SectionState) {
	return render(SectionStateLine, { status });
}

describe('SectionStateLine', () => {
	it.each(cases)('says how the section stands, case %#', async (status, line) => {
		await show(status);
		await expect.element(page.getByText(line)).toBeVisible();
	});

	it('names no service when none is the cause', async () => {
		await show({ key: KEY, zone: ZONE, state: 'source_down' });
		await expect.element(page.getByText(/^A music service isn't answering\./)).toBeVisible();
	});

	it('offers a way to follow artists when a section is empty', async () => {
		await show({ key: KEY, zone: ZONE, state: 'empty' });
		await expect
			.element(page.getByRole('link', { name: 'Follow artists' }))
			.toHaveAttribute('href', expect.stringMatching(/\/search$/));
	});

	it('offers no follow for a library shelf', async () => {
		await show({ key: 'anniversaries', zone: 'from_your_library', state: 'empty' });
		await expect.element(page.getByText(/^Nothing here yet\.$/)).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Follow artists' }))
			.not.toBeInTheDocument();
	});

	it('links to the account a section waits on', async () => {
		await show({ key: KEY, zone: ZONE, state: 'needs_link', source: 'listenbrainz' });
		await expect
			.element(page.getByRole('link', { name: 'Connect ListenBrainz' }))
			.toHaveAttribute('href', expect.stringMatching(/\/profile#scrobbling$/));
	});
});
