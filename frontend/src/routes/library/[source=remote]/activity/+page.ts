import { error } from '@sveltejs/kit';
import { remoteSources } from '$lib/features/remote-library/queries/remoteSources';
import type { PageLoad } from './$types';

export const load: PageLoad = ({ params }) => {
	if (!remoteSources[params.source].capabilities.activity) error(404, 'Not Found');
};
