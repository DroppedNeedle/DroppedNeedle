<script lang="ts">
	import { resolve } from '$app/paths';
	import { serviceStatusStore } from '$lib/stores/serviceStatus';
	import { fromStore } from 'svelte/store';
	import { PersistedState } from 'runed';
	import { ArrowUpCircle, X } from 'lucide-svelte';

	interface Props {
		updateAvailable: boolean;
		latestVersion: string | null;
	}
	let { updateAvailable, latestVersion }: Props = $props();

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
			class="alert alert-info shadow-lg mx-auto mt-2 max-w-xl pointer-events-auto text-sm gap-2 py-2 banner-enter"
			role="status"
		>
			<ArrowUpCircle class="h-4 w-4 shrink-0" />
			<span>
				A new version of DroppedNeedle is available
				{#if latestVersion}
					<span class="font-semibold">({latestVersion})</span>
				{/if}
			</span>
			<a href={resolve('/settings?tab=about')} class="btn btn-accent btn-xs btn-outline">Details</a>
			<button
				class="btn btn-ghost btn-sm btn-circle"
				onclick={dismiss}
				aria-label="Dismiss update notification"
			>
				<X class="h-3 w-3" />
			</button>
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
</style>
