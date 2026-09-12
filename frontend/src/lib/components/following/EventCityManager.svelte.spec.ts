import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { EventCity } from '$lib/queries/following/types';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const mutate = vi.fn();

vi.mock('$lib/queries/following/FollowMutations.svelte', () => ({
	createReplaceEventCitiesMutation: () => ({ mutate })
}));

// CitySearchInput's query hook (rendered when "Add city" is opened)
vi.mock('$lib/queries/following/FollowQueries.svelte', () => ({
	getCitySearchQuery: () => ({ data: { items: [] }, isFetching: false, isError: false })
}));

import EventCityManager from './EventCityManager.svelte';

const LIVERPOOL: EventCity = {
	city_name: 'Liverpool',
	latitude: 53.41,
	longitude: -2.98,
	radius_km: 32,
	country_code: 'GB'
};
const CHESTER: EventCity = {
	city_name: 'Chester',
	latitude: 53.19,
	longitude: -2.89,
	radius_km: 80,
	country_code: 'GB'
};

describe('EventCityManager', () => {
	beforeEach(() => {
		mutate.mockClear();
	});

	it('renders a chip per city with its radius in miles', async () => {
		await render(EventCityManager, {
			props: { cities: [LIVERPOOL, CHESTER] }
		} as Parameters<typeof render<typeof EventCityManager>>[1]);
		// chips carry the radius; the (closed) preset dropdowns repeat the text,
		// so assert on the chip buttons via their title
		await expect.element(page.getByTitle('Within 20 mi of Liverpool')).toBeVisible(); // 32 km
		await expect.element(page.getByTitle('Within 50 mi of Chester')).toBeVisible(); // 80 km
	});

	it('opens the city search when Add city is clicked', async () => {
		await render(EventCityManager, {
			props: { cities: [LIVERPOOL] }
		} as Parameters<typeof render<typeof EventCityManager>>[1]);
		await page.getByRole('button', { name: 'Add city' }).click();
		await expect.element(page.getByLabelText('Search a city')).toBeVisible();
	});

	it('removing a city submits the remaining list', async () => {
		await render(EventCityManager, {
			props: { cities: [LIVERPOOL, CHESTER] }
		} as Parameters<typeof render<typeof EventCityManager>>[1]);
		await page.getByTitle(/of Liverpool/).click();
		await page.getByRole('button', { name: 'Remove city' }).first().click();
		expect(mutate).toHaveBeenCalledWith([CHESTER]);
	});

	it('picking a radius preset stores kilometres', async () => {
		await render(EventCityManager, {
			props: { cities: [LIVERPOOL] }
		} as Parameters<typeof render<typeof EventCityManager>>[1]);
		await page.getByTitle(/of Liverpool/).click();
		await page.getByRole('button', { name: 'within 50 mi' }).click();
		expect(mutate).toHaveBeenCalledWith([expect.objectContaining({ radius_km: 80 })]);
	});
});
