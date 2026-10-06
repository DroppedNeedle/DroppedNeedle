import { v3 } from '$lib/api/v3/endpoint';

// The contract declares no request body for the config save, test and
// import routes, so those writes go through the untyped client with the
// generated response type.
export const LIDARR_IMPORT_ENDPOINTS = {
	config: () => v3('/api/v3/acquire/lidarr-import/config'),
	test: () => v3('/api/v3/acquire/lidarr-import/test'),
	artists: () => v3('/api/v3/acquire/lidarr-import/artists'),
	import: () => v3('/api/v3/acquire/lidarr-import/import')
} as const;
