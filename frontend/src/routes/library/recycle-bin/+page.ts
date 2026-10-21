import { withBasePath } from '$lib/utils/basePath';
import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

export const load: PageLoad = async ({ parent }) => {
	const { user } = await parent();
	if (user?.role !== 'admin') throw redirect(302, withBasePath('/library'));
	return {};
};
