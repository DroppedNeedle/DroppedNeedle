import type { AlbumRef, ArtistRef, ListenSource, StatsRange } from '$lib/queries/discover/types';
import { key, userScope } from '$lib/queries/keys';
import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { PERSIST } from '$lib/queries/persistedCache';

/** The three ranked chart pages under /home. */
export type ChartKind = 'trending-artists' | 'popular-albums' | 'your-top-albums';

export interface ChartPageParams {
	range: StatsRange;
	limit: number;
	offset: number;
	source: ListenSource | null;
}

export function chartQuery({ range, limit, offset, source }: ChartPageParams) {
	return source ? { range, limit, offset, source } : { range, limit, offset };
}

type UserId = string | null | undefined;

export const ChartQueryKeyFactory = {
	chart: (userId: UserId, chart: ChartKind, source: ListenSource | null) =>
		key(userScope(userId), 'charts', chart, source),
	overview: (userId: UserId, chart: ChartKind, source: ListenSource | null) =>
		[...ChartQueryKeyFactory.chart(userId, chart, source), 'overview'] as const,
	range: (userId: UserId, chart: ChartKind, source: ListenSource | null, range: StatsRange) =>
		[...ChartQueryKeyFactory.chart(userId, chart, source), 'range', range] as const
};

type Getter<T> = () => T;
export type ChartItem = AlbumRef | ArtistRef;

const CHART_RANGES: readonly StatsRange[] = ['this_week', 'this_month', 'this_year', 'all_time'];

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

export type ChartOverview = Record<StatsRange, ChartRangeOverview>;

const OVERVIEW_LIMIT = 10;
const PAGE_SIZE = 25;

async function fetchChartPage(
	chart: ChartKind,
	params: ChartPageParams,
	signal?: AbortSignal
): Promise<ChartPage> {
	if (chart === 'trending-artists') {
		return api.v3.GET(v3('/api/v3/home/trending/artists', { query: chartQuery(params) }), {
			signal
		});
	}
	const url =
		chart === 'popular-albums'
			? v3('/api/v3/home/popular/albums', { query: chartQuery(params) })
			: v3('/api/v3/home/your-top/albums', { query: chartQuery(params) });
	return api.v3.GET(url, { signal });
}

// The overview shows every range at once: one short page per range.
export const getChartOverviewQuery = (
	getChart: Getter<ChartKind>,
	getSource: Getter<ListenSource | null>
) =>
	createQuery(() => ({
		meta: PERSIST,
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
	getSource: Getter<ListenSource | null>,
	getRange: Getter<StatsRange | null>
) =>
	createInfiniteQuery(() => ({
		meta: PERSIST,
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
