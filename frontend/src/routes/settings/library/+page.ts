import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

// Library settings moved into the consolidated Settings → Library tab.
export const load: PageLoad = async ({ parent }) => {
	const { user } = await parent();
	if (user?.role !== 'admin') {
		throw redirect(302, '/');
	}
	throw redirect(307, '/settings?tab=library');
};
