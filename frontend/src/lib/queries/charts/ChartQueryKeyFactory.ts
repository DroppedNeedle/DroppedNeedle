import { key, userScope } from '../keys';
import type { ListenSource, StatsRange } from '../discover/types';
import type { ChartKind } from './endpoints';

type UserId = string | null | undefined;

export const ChartQueryKeyFactory = {
	chart: (userId: UserId, chart: ChartKind, source: ListenSource | null) =>
		key(userScope(userId), 'charts', chart, source),
	overview: (userId: UserId, chart: ChartKind, source: ListenSource | null) =>
		[...ChartQueryKeyFactory.chart(userId, chart, source), 'overview'] as const,
	range: (userId: UserId, chart: ChartKind, source: ListenSource | null, range: StatsRange) =>
		[...ChartQueryKeyFactory.chart(userId, chart, source), 'range', range] as const
};
