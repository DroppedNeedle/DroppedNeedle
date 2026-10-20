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

it('names the service that is down and when to try again', () => {
	const error = new ApiError(
		503,
		'An outside service is not answering. Try again later.',
		'PROVIDER_UNAVAILABLE',
		{ error_id: 'req-1', provider: 'listenbrainz', retry_after: 30 }
	);
	const view = describeServiceError(error, 'Home');

	expect(view.kind).toBe('provider_down');
	expect(view.message).toBe('ListenBrainz is not answering. Try again in 30 seconds.');
	expect(view.canRetry).toBe(true);
});
