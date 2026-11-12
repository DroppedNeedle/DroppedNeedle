// Which connected services get an entry in the navigation: the rail and the
// phone's More sheet read the same answer.
import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
import { getSectionPrefsQuery } from '$lib/features/settings/queries/section-prefs.svelte';
import type { ServiceSource } from '$lib/shell/navItems';

export function useServiceEntries() {
	const integrations = useIntegrationStatus();
	const prefsQuery = getSectionPrefsQuery();
	// fail-open: until prefs load, every service is visible
	const hidden = $derived(
		(prefsQuery.data?.pages?.sidebar ?? []).filter((s) => !s.enabled).map((s) => s.key)
	);

	return {
		get integrations() {
			return integrations.current;
		},
		/** The person hid this service from the navigation. */
		isHidden: (source: ServiceSource): boolean => hidden.includes(source),
		/** Set up and not hidden: its page gets an entry. */
		hasEntry: (source: ServiceSource): boolean =>
			integrations.current[source] && !hidden.includes(source)
	};
}
