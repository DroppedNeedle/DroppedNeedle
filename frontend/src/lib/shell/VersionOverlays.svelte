<script lang="ts">
	import {
		getUpdateCheckQuery,
		getVersionQuery,
		getReleaseHistoryQuery
	} from '$lib/shell/version.svelte';
	import { loadWhatsNewModal } from '$lib/shell/lazyComponentLoaders';
	import UpdateBanner from '$lib/shell/UpdateBanner.svelte';
	import type WhatsNewModalComponent from '$lib/shell/WhatsNewModal.svelte';
	import { isWhatsNewDismissed } from '$lib/stores/version.svelte';

	// eslint-disable-next-line no-useless-assignment -- bindable default is the parent-visible initial value; this prop is write-only in-component by design
	let { updateAvailable = $bindable(false) }: { updateAvailable: boolean } = $props();

	const updateCheckQuery = getUpdateCheckQuery();
	const versionQuery = getVersionQuery();

	const currentVersion = $derived(versionQuery.data?.version ?? null);
	const buildDate = $derived(versionQuery.data?.build_date ?? null);
	const isDev = $derived(currentVersion === 'dev');

	// The key What's New is closed under for this build, known before the
	// release history: the build date on a dev build, else the version. Once
	// it is closed the history is not read again.
	const buildKey = $derived(
		currentVersion === null ? null : isDev ? (buildDate ?? 'dev') : currentVersion
	);
	const releaseHistoryQuery = getReleaseHistoryQuery(
		() => buildKey !== null && !isWhatsNewDismissed(buildKey)
	);

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

	// When What's New shows: notes for this release not yet closed. Worked out
	// here only, so the modal's code loads only when it will open.
	const dismissKey = $derived(
		isDev ? (buildDate ?? 'dev') : (minorReleases[0]?.tag_name ?? currentVersion)
	);
	const hasNotes = $derived(
		currentVersion !== null &&
			dismissKey !== null &&
			minorReleases.some((r) => r.body && r.body.trim().length > 0) &&
			!isWhatsNewDismissed(dismissKey)
	);

	let WhatsNewModal = $state<typeof WhatsNewModalComponent | null>(null);
	$effect(() => {
		if (!hasNotes || WhatsNewModal) return;
		let cancelled = false;
		loadWhatsNewModal()
			.then((component) => {
				if (!cancelled) WhatsNewModal = component;
			})
			// Not shown this time; the next load tries again.
			.catch(() => undefined);
		return () => {
			cancelled = true;
		};
	});

	$effect(() => {
		updateAvailable = updateCheckQuery.data?.update_available ?? false;
	});
</script>

<UpdateBanner
	updateAvailable={updateCheckQuery.data?.update_available ?? false}
	latestVersion={updateCheckQuery.data?.latest_version ?? null}
	releaseUrl={updateCheckQuery.data?.latest_release?.html_url ?? null}
/>
{#if WhatsNewModal && hasNotes && dismissKey}
	<WhatsNewModal {currentVersion} releases={minorReleases} {dismissKey} />
{/if}
