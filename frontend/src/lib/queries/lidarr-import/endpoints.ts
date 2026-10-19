import { v3 } from '$lib/api/v3/endpoint';

export const LIDARR_IMPORT_ENDPOINTS = {
	config: () => v3('/api/v3/acquire/lidarr-import/config'),
	test: () => v3('/api/v3/acquire/lidarr-import/test'),
	artists: () => v3('/api/v3/acquire/lidarr-import/artists'),
	import: () => v3('/api/v3/acquire/lidarr-import/import')
} as const;
