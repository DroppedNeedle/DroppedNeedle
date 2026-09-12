<script lang="ts">
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import {
		Users,
		Disc3,
		Image,
		LoaderCircle,
		Check,
		TriangleAlert,
		X,
		Search,
		Minus,
		ChevronUp
	} from 'lucide-svelte';

	const phaseIcons: Record<string, typeof Users> = {
		artists: Users,
		discovery: Search,
		albums: Disc3,
		audiodb_prewarm: Image
	};

	let PhaseIcon = $derived(syncStatus.phase ? (phaseIcons[syncStatus.phase] ?? LoaderCircle) : LoaderCircle);

	let isComplete = $derived(!syncStatus.isActive && !syncStatus.error && syncStatus.showIndicator);

	let pillLabel = $derived.by(() => {
		if (isComplete) return 'Sync Complete';
		if (syncStatus.error) return 'Sync Failed';
		return `${syncStatus.phaseLabel}… ${syncStatus.progress}%`;
	});
</script>

{#if syncStatus.showIndicator}
	<div
		class="fixed right-4 z-40 w-80 max-w-[calc(100vw-2rem)]"
		class:bottom-24={playerStore.isPlayerVisible}
		class:bottom-4={!playerStore.isPlayerVisible}
		role="status"
		aria-live="polite"
		aria-label="Library sync progress"
	>
		{#if syncStatus.isMinimized}
			<!-- Minimized pill -->
			<button
				class="flex items-center gap-2 bg-base-200 rounded-full shadow-xl border border-base-300 pl-3 pr-2 py-1.5 w-auto max-w-full cursor-pointer hover:bg-base-300/80 transition-colors animate-slide-in-right ml-auto"
				onclick={() => syncStatus.expand()}
				aria-label="Expand sync progress"
			>
				<div class="shrink-0">
					{#if isComplete}
						<Check class="h-3.5 w-3.5 text-success" />
					{:else if syncStatus.error}
						<TriangleAlert class="h-3.5 w-3.5 text-error" />
					{:else}
						<PhaseIcon
							class="h-3.5 w-3.5 text-primary {syncStatus.isActive ? 'animate-pulse' : ''}"
						/>
					{/if}
				</div>
				<span
					class="text-xs font-medium truncate"
					class:text-success={isComplete}
					class:text-error={syncStatus.error}
				>
					{pillLabel}
				</span>
				<ChevronUp class="h-3.5 w-3.5 opacity-50 shrink-0" />
			</button>
		{:else}
			<!-- Expanded card -->
			<div
				class="bg-base-200 rounded-box shadow-xl border border-base-300 p-4 animate-slide-in-right"
			>
				<div class="flex items-center justify-between gap-2 mb-3">
					<div class="flex items-center gap-2.5 min-w-0">
						{#if isComplete}
							<div class="bg-success/15 rounded-full p-1.5 shrink-0">
								<Check class="h-4 w-4 text-success" />
							</div>
							<span class="font-semibold text-sm text-success">Sync Complete</span>
						{:else if syncStatus.error}
							<div class="bg-error/15 rounded-full p-1.5 shrink-0">
								<TriangleAlert class="h-4 w-4 text-error" />
							</div>
							<span class="font-semibold text-sm text-error">Sync Failed</span>
						{:else}
							<div class="bg-primary/10 rounded-full p-1.5 shrink-0">
								<PhaseIcon
									class="h-4 w-4 text-primary {syncStatus.isActive ? 'animate-pulse' : ''}"
								/>
							</div>
							<div class="min-w-0">
								<span class="font-semibold text-sm">{syncStatus.phaseLabel}</span>
								{#if syncStatus.phaseNumber > 0}
									<span class="text-xs text-base-content/40 ml-1">
										{syncStatus.phaseNumber}/{syncStatus.totalPhases}
									</span>
								{/if}
							</div>
						{/if}
					</div>
					<div class="flex items-center gap-0.5 shrink-0">
						<button
							class="btn btn-ghost btn-xs btn-circle opacity-50 hover:opacity-100"
							onclick={() => syncStatus.minimize()}
							aria-label="Minimize sync progress"
							title="Minimize"
						>
							<Minus class="h-3.5 w-3.5" />
						</button>
						<button
							class="btn btn-ghost btn-xs btn-circle opacity-50 hover:opacity-100"
							onclick={() => syncStatus.dismiss()}
							aria-label="Dismiss notification"
							title="Dismiss"
						>
							<X class="h-3.5 w-3.5" />
						</button>
					</div>
				</div>

				{#if syncStatus.isActive}
					<div
						class="w-full bg-base-300 rounded-full h-1.5 mb-2 overflow-hidden"
						role="progressbar"
						aria-valuenow={syncStatus.progress}
						aria-valuemin={0}
						aria-valuemax={100}
						aria-label="{syncStatus.phaseLabel} progress"
					>
						<div
							class="h-full rounded-full bg-primary transition-all duration-700 ease-out"
							style="width: {syncStatus.progress}%"
						></div>
					</div>

					<div class="flex justify-between items-center text-xs text-base-content/50">
						{#if syncStatus.totalItems === 0}
							<span>Cached ✓</span>
						{:else}
							<span>{syncStatus.processedItems} / {syncStatus.totalItems}</span>
							<span>{syncStatus.progress}%</span>
						{/if}
					</div>

					{#if syncStatus.currentItem}
						<div class="text-xs text-base-content/35 truncate mt-1">
							{syncStatus.currentItem}
						</div>
					{/if}
				{/if}

				{#if syncStatus.error}
					<p class="text-xs text-error/70 truncate mt-1">{syncStatus.error}</p>
				{/if}
			</div>
		{/if}
	</div>
{/if}

<style>
	@keyframes slide-in-right {
		from {
			opacity: 0;
			transform: translateX(110%);
		}
		to {
			opacity: 1;
			transform: translateX(0);
		}
	}
	.animate-slide-in-right {
		animation: slide-in-right 0.35s cubic-bezier(0.16, 1, 0.3, 1);
	}
</style>
