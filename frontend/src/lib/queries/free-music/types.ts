import type { components } from '$lib/api/v3/openapi';

// The generated v3 Free Music schemas, with the values the server sends
// spelled out.

export type FreeMusicStatus =
	'searching' | 'downloading' | 'importing' | 'completed' | 'failed' | 'cancelled';

export type FreeMusicTask = Omit<
	components['schemas']['FreeMusicTaskResponse'],
	'status' | 'kind'
> & {
	status: FreeMusicStatus;
	kind: 'album' | 'track';
};

export interface FreeMusicTasks {
	tasks: FreeMusicTask[];
}
