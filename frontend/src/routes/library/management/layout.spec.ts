import { describe, expect, it } from 'vitest';

import { load } from './+layout';

describe('Library Management authorization', () => {
	it('admits administrators and sends anyone else away before the desk renders', async () => {
		await expect(
			load({ parent: async () => ({ user: { role: 'admin' } }) } as never)
		).resolves.toEqual({});
		for (const role of ['trusted', 'user']) {
			await expect(
				load({ parent: async () => ({ user: { role } }) } as never),
				role
			).rejects.toMatchObject({ status: 302, location: '/library' });
		}
	});
});
