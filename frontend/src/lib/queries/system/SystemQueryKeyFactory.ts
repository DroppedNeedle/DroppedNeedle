import { key, SHARED } from '../keys';

// Service health is the server's, the same for everyone.
export const SystemQueryKeyFactory = {
	health: () => key(SHARED, 'system', 'health')
};
