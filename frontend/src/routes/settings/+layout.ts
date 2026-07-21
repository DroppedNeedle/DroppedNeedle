import { redirect } from '@sveltejs/kit';
import type { LayoutLoad } from './$types';

export const ssr = false;

export const load: LayoutLoad = async ({ parent }) => {
	const { user } = await parent();
	if (user?.role !== 'admin') {
		throw redirect(302, '/');
	}
};
