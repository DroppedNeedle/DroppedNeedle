<script lang="ts">
	import { serviceStatusStore } from '$lib/stores/serviceStatus';
	import { fromStore } from 'svelte/store';
	import { PersistedState } from 'runed';
	import { CircleArrowUp, X } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { outsideLink } from '$lib/ui/outsideLink';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		updateAvailable: boolean;
		latestVersion: string | null;
		/** The release's GitHub page, where non-admins read the details. */
		releaseUrl: string | null;
	}
	let { updateAvailable, latestVersion, releaseUrl }: Props = $props();

	const status = fromStore(serviceStatusStore);

	const dismissedVersion = new PersistedState<string | null>(
		'droppedneedle_update_banner_dismissed',
		null
	);

	const degradedSources = $derived(Object.keys(status.current));
	const hasDegradation = $derived(degradedSources.length > 0);

	const showBanner = $derived(
		updateAvailable && latestVersion !== null && dismissedVersion.current !== latestVersion
	);

	// Non-admins open the release page; the About tab is admin only.
	const releaseLink = $derived(outsideLink(releaseUrl));

	function dismiss() {
		dismissedVersion.current = latestVersion;
	}
</script>

{#if showBanner}
	<div
		class="fixed top-0 left-0 right-0 z-[114] flex items-center justify-center pointer-events-none"
		class:mt-12={hasDegradation}
	>
		<div
			class="alert border-line bg-base-200 shadow-lg mx-auto mt-2 max-w-xl pointer-events-auto text-sm gap-2 py-2 banner-enter"
			role="status"
		>
			<CircleArrowUp class="h-4 w-4 shrink-0 text-info" />
			<span>
				A new version of DroppedNeedle is available
				{#if latestVersion}
					<span class="font-semibold">({latestVersion})</span>
				{/if}
			</span>
			{#if authStore.isAdmin}
				<Button variant="info" size="xs" href={withBasePath('/settings?tab=about')}>Details</Button>
			{:else if releaseLink}
				<Button
					variant="info"
					size="xs"
					href={releaseLink}
					target="_blank"
					rel="noopener noreferrer"
				>
					Details
				</Button>
			{/if}
			<IconButton size="sm" icon={X} label="Dismiss update notification" onclick={dismiss} />
		</div>
	</div>
{/if}

<style>
	@keyframes slide-down {
		0% {
			opacity: 0;
			transform: translateY(-100%);
		}
		100% {
			opacity: 1;
			transform: translateY(0);
		}
	}
	.banner-enter {
		animation: slide-down 0.3s ease-out forwards;
	}

	@media (prefers-reduced-motion: reduce) {
		.banner-enter {
			opacity: 1 !important;
		}
	}
</style>
