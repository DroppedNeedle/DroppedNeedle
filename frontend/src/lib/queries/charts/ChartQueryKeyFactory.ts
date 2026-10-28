import { userIdSegment } from '../userKeySegment';
import type { ListenSource, StatsRange } from '../discover/types';
import type { ChartKind } from './endpoints';

export const ChartQueryKeyFactory = {
	prefix: ['charts'] as const,
	chart: (userId: string | null | undefined, chart: ChartKind, source: ListenSource | null) =>
		[...ChartQueryKeyFactory.prefix, userIdSegment(userId), chart, source] as const,
	overview: (userId: string | null | undefined, chart: ChartKind, source: ListenSource | null) =>
		[...ChartQueryKeyFactory.chart(userId, chart, source), 'overview'] as const,
	range: (
		userId: string | null | undefined,
		chart: ChartKind,
		source: ListenSource | null,
		range: StatsRange
	) => [...ChartQueryKeyFactory.chart(userId, chart, source), 'range', range] as const
};
