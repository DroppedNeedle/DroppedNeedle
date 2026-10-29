import { key, userScope } from '../keys';

export const SectionPrefsQueryKeyFactory = {
	prefs: (userId: string | null | undefined) => key(userScope(userId), 'section-prefs')
};
