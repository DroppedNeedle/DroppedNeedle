import { userIdSegment } from '../userKeySegment';
import type { ChartKind, ChartRange, ChartSource } from './endpoints';

export const ChartQueryKeyFactory = {
	prefix: ['charts'] as const,
	chart: (userId: string | null | undefined, chart: ChartKind, source: ChartSource | null) =>
		[...ChartQueryKeyFactory.prefix, userIdSegment(userId), chart, source] as const,
	overview: (userId: string | null | undefined, chart: ChartKind, source: ChartSource | null) =>
		[...ChartQueryKeyFactory.chart(userId, chart, source), 'overview'] as const,
	range: (
		userId: string | null | undefined,
		chart: ChartKind,
		source: ChartSource | null,
		range: ChartRange
	) => [...ChartQueryKeyFactory.chart(userId, chart, source), 'range', range] as const
};
