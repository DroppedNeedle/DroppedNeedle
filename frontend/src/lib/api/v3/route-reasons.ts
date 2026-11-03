/**
 * Operations in the API document that no builder in the web app names, each
 * with the reason it stays. `route-coverage.spec.ts` reads this list.
 */
export const ROUTE_REASONS: readonly { method: string; path: string; reason: string }[] = [
	{ method: 'GET', path: '/health', reason: 'container health check' },
	{ method: 'GET', path: '/openapi.json', reason: 'the served API document' },
	{
		method: 'POST',
		path: '/api/v3/auth/device-sessions',
		reason: 'sign-in for devices and scripts'
	},
	{ method: 'GET', path: '/api/v3/wrapped/server', reason: 'key-gated API for outside tools' },
	{ method: 'GET', path: '/api/v3/wrapped/users', reason: 'key-gated API for outside tools' },
	{
		method: 'GET',
		path: '/api/v3/wrapped/user/{user_id}',
		reason: 'key-gated API for outside tools'
	},
	{
		method: 'GET',
		path: '/api/v3/library/contributions/musicbrainz/callback',
		reason: 'MusicBrainz sends the browser back here'
	},
	{
		method: 'GET',
		path: '/api/v3/auth/oidc/callback',
		reason: 'the identity provider sends the browser back here'
	},
	{
		method: 'GET',
		path: '/api/v3/acquire/spotify/auth/callback',
		reason: 'Spotify sends the browser back here'
	},
	{ method: 'POST', path: '/api/v3/plugins/ext/{name}/{subpath}', reason: 'plugin routes' },
	{ method: 'DELETE', path: '/api/v3/plugins/ext/{name}/{subpath}', reason: 'plugin routes' },
	{
		method: 'GET',
		path: '/api/v3/stream/{source}/{*key}',
		reason: 'audio URL built by hand: a Plex key keeps its slashes'
	},
	{
		method: 'HEAD',
		path: '/api/v3/stream/{source}/{*key}',
		reason: 'audio URL built by hand: a Plex key keeps its slashes'
	},
	{
		method: 'GET',
		path: '/api/v3/users/{id}/avatar',
		reason: 'the server hands the browser this URL'
	},
	{
		method: 'GET',
		path: '/api/v3/remotes/{source}/images/{id}',
		reason: 'the server hands the browser this URL'
	},
	{
		method: 'GET',
		path: '/api/v3/remotes/{source}/covers/playlists/{id}',
		reason: 'the server hands the browser this URL'
	},
	...[
		['GET', '/api/v3/upgrade/state'],
		['POST', '/api/v3/upgrade/sign-in'],
		['POST', '/api/v3/upgrade/preflight'],
		['POST', '/api/v3/upgrade/start'],
		['POST', '/api/v3/upgrade/fresh'],
		['GET', '/api/v3/upgrade/progress'],
		['POST', '/api/v3/upgrade/undo'],
		['POST', '/api/v3/upgrade/continue']
	].map(([method, path]) => ({
		method,
		path,
		reason: 'the upgrade page, served by a server started on v2 data'
	}))
];
