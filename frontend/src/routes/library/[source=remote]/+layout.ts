import { remoteSources } from '$lib/queries/remotes/remoteSources';
import type { LayoutLoad } from './$types';

export const load: LayoutLoad = ({ params }) => ({ remote: remoteSources[params.source] });
