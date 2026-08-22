import { base } from '$app/paths';

export function stripBase(pathname: string): string {
	if (!base || !pathname.startsWith(base)) return pathname;
	return pathname.slice(base.length) || '/';
}
