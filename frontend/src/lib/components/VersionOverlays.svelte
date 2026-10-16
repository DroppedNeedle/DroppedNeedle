<script lang="ts">
	import {
		getUpdateCheckQuery,
		getVersionQuery,
		getReleaseHistoryQuery
	} from '$lib/queries/VersionQuery.svelte';
	import UpdateBanner from '$lib/components/UpdateBanner.svelte';
	import WhatsNewModal from '$lib/components/WhatsNewModal.svelte';

	// eslint-disable-next-line no-useless-assignment -- bindable default is the parent-visible initial value; this prop is write-only in-component by design
	let { updateAvailable = $bindable(false) }: { updateAvailable: boolean } = $props();

	const updateCheckQuery = getUpdateCheckQuery();
	const versionQuery = getVersionQuery();
	const releaseHistoryQuery = getReleaseHistoryQuery();

	const currentVersion = $derived(versionQuery.data?.version ?? null);
	const buildDate = $derived(versionQuery.data?.build_date ?? null);
	const isDev = $derived(currentVersion === 'dev');

	function getMinorPrefix(tag: string): string | null {
		const m = tag.replace(/^v/, '').match(/^(\d+\.\d+)\./);
		return m ? m[1] : null;
	}

	// Collect releases sharing the same minor version, up to and including the current version
	const minorReleases = $derived.by(() => {
		const releases = releaseHistoryQuery.data;
		if (!releases || releases.length === 0) return [];

		if (isDev) {
			const prefix = getMinorPrefix(releases[0].tag_name);
			if (!prefix) return [releases[0]];
			return releases.filter((r) => !r.prerelease && getMinorPrefix(r.tag_name) === prefix);
		}

		if (!currentVersion) return [];

		const prefix = getMinorPrefix(currentVersion);
		if (!prefix) {
			const exact = releases.find((r) => r.tag_name === currentVersion);
			return exact ? [exact] : [];
		}

		// Same minor, then slice from the current version downward (releases are newest-first)
		const sameMinor = releases.filter(
			(r) => !r.prerelease && getMinorPrefix(r.tag_name) === prefix
		);
		const currentIdx = sameMinor.findIndex((r) => r.tag_name === currentVersion);
		if (currentIdx === -1) return sameMinor;
		return sameMinor.slice(currentIdx);
	});

	$effect(() => {
		updateAvailable = updateCheckQuery.data?.update_available ?? false;
	});
</script>

<UpdateBanner
	updateAvailable={updateCheckQuery.data?.update_available ?? false}
	latestVersion={updateCheckQuery.data?.latest_version ?? null}
/>
<WhatsNewModal {currentVersion} {buildDate} releases={minorReleases} />
