import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import type { HomeAlbum, HomeArtist } from '$lib/types';
import { toAlbum, toArtist } from '../chartAdapters';
import { ChartQueryKeyFactory } from './ChartQueryKeyFactory';
import {
	HOME_ENDPOINTS,
	type ChartKind,
	type ChartPageParams,
	type ChartRange,
	type ChartSource
} from './endpoints';

type Getter<T> = () => T;
export type ChartItem = HomeAlbum | HomeArtist;

export const CHART_RANGES: readonly ChartRange[] = [
	'this_week',
	'this_month',
	'this_year',
	'all_time'
];

export interface ChartPage {
	items: ChartItem[];
	offset: number;
	limit: number;
	has_more: boolean;
}

/** One range on the overview: the top item stands out, the next ones follow. */
export interface ChartRangeOverview {
	featured: ChartItem | null;
	items: ChartItem[];
}

export type ChartOverview = Record<ChartRange, ChartRangeOverview>;

const OVERVIEW_LIMIT = 10;
const PAGE_SIZE = 25;

async function fetchChartPage(
	chart: ChartKind,
	params: ChartPageParams,
	signal?: AbortSignal
): Promise<ChartPage> {
	if (chart === 'trending-artists') {
		const page = await api.global.v3.GET(HOME_ENDPOINTS.trendingArtists(params), { signal });
		return { ...page, items: page.items.map(toArtist) };
	}
	const url =
		chart === 'popular-albums'
			? HOME_ENDPOINTS.popularAlbums(params)
			: HOME_ENDPOINTS.yourTopAlbums(params);
	const page = await api.global.v3.GET(url, { signal });
	return { ...page, items: page.items.map(toAlbum) };
}

// The overview shows every range at once: one short page per range.
export const getChartOverviewQuery = (
	getChart: Getter<ChartKind>,
	getSource: Getter<ChartSource | null>
) =>
	createQuery(() => ({
		staleTime: CACHE_TTL.TIME_RANGE_OVERVIEW,
		queryKey: ChartQueryKeyFactory.overview(authStore.user?.id, getChart(), getSource()),
		queryFn: async ({ signal }): Promise<ChartOverview> => {
			const chart = getChart();
			const source = getSource();
			const pages = await Promise.all(
				CHART_RANGES.map((range) =>
					fetchChartPage(chart, { range, limit: OVERVIEW_LIMIT, offset: 0, source }, signal)
				)
			);
			const entries = CHART_RANGES.map((range, index) => {
				const items = pages[index]?.items ?? [];
				return [range, { featured: items[0] ?? null, items: items.slice(1) }] as const;
			});
			return Object.fromEntries(entries) as ChartOverview;
		}
	}));

// The expanded range pages through the full chart.
export const getChartRangeQuery = (
	getChart: Getter<ChartKind>,
	getSource: Getter<ChartSource | null>,
	getRange: Getter<ChartRange | null>
) =>
	createInfiniteQuery(() => ({
		staleTime: CACHE_TTL.TIME_RANGE_OVERVIEW,
		queryKey: ChartQueryKeyFactory.range(
			authStore.user?.id,
			getChart(),
			getSource(),
			getRange() ?? 'this_week'
		),
		enabled: getRange() !== null,
		initialPageParam: 0,
		queryFn: ({ pageParam, signal }) =>
			fetchChartPage(
				getChart(),
				{
					range: getRange() ?? 'this_week',
					limit: PAGE_SIZE,
					offset: pageParam,
					source: getSource()
				},
				signal
			),
		getNextPageParam: (lastPage: ChartPage) =>
			lastPage.has_more ? lastPage.offset + lastPage.limit : undefined
	}));
