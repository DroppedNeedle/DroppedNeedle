import { remoteSources } from '$lib/features/remote-library/queries/remoteSources';
import type { LayoutLoad } from './$types';

export const load: LayoutLoad = ({ params }) => ({ remote: remoteSources[params.source] });
