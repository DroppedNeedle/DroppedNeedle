import { expect, it } from 'vitest';

import { ApiError } from '$lib/api/client';
import { describeServiceError } from './serviceError';

it('shows a 409 NOT_CONFIGURED read as not set up, with the server sentence', () => {
	const sentence = 'Add a Last.fm key in Settings to see charts.';
	const view = describeServiceError(new ApiError(409, sentence, 'NOT_CONFIGURED'), 'Discover');

	expect(view.kind).toBe('not_configured');
	expect(view.title).toBe('Not set up yet');
	expect(view.message).toBe(sentence);
	expect(view.canRetry).toBe(false);
	expect(view.settingsHref).toBe('/settings');
});
