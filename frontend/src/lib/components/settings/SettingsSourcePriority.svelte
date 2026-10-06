<script lang="ts">
	import {
		ChevronDown,
		ChevronUp,
		GripVertical,
		HardDriveDownload,
		Puzzle,
		Rss
	} from 'lucide-svelte';

	import {
		getNzbgetConfigQuery,
		getSabnzbdConfigQuery,
		getSourcePriorityQuery,
		saveSourcePriority
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { getPluginSourcesQuery } from '$lib/queries/plugins/PluginSourceQueries.svelte';
	import { toastStore } from '$lib/stores/toast';

	const priorityQuery = getSourcePriorityQuery();
	const reorder = saveSourcePriority();
	const sourcesQuery = getPluginSourcesQuery();
	const sabnzbdQuery = getSabnzbdConfigQuery();
	const nzbgetQuery = getNzbgetConfigQuery();

	// Usenet is one source served by one of two clients, so name whichever is actually
	// enabled. SABnzbd takes precedence when both are, matching the backend's choice.
	const usenetClient = $derived(
		sabnzbdQuery.data?.enabled
			? 'SABnzbd'
			: nzbgetQuery.data?.enabled
				? 'NZBGet'
				: 'SABnzbd or NZBGet'
	);

	const META: Record<string, { label: string; sub: string; icon: typeof Rss }> = {
		soulseek: { label: 'Soulseek', sub: 'slskd', icon: HardDriveDownload }
	};

	const pluginLabels = $derived(
		Object.fromEntries(
			(sourcesQuery.data?.sources ?? []).map((source) => [
				source.key,
				source.display_name || source.key
			])
		)
	);
	const knownKeys = $derived([
		'soulseek',
		'usenet',
		...(sourcesQuery.data?.sources ?? []).map((source) => source.key)
	]);
	// Saved order wins; newly installed plugin keys append, and keys from removed
	// plugins stay visible (greyed) so the saved order never silently reorders.
	const savedOrder = $derived(priorityQuery.data?.order ?? knownKeys);
	const order = $derived([...savedOrder, ...knownKeys.filter((key) => !savedOrder.includes(key))]);

	function metaFor(source: string) {
		if (source === 'usenet')
			return { label: 'Usenet', sub: usenetClient, icon: Rss, missing: false };
		const base = META[source];
		if (base) return { ...base, missing: false };
		if (pluginLabels[source])
			return { label: pluginLabels[source], sub: 'Plugin', icon: Puzzle, missing: false };
		return { label: source, sub: 'Removed', icon: Puzzle, missing: true };
	}

	let dragSource = $state<string | null>(null);

	function persist(next: string[]) {
		const roster = sourcesQuery.data?.sources;
		const payload = roster
			? next.filter(
					(key) =>
						key === 'soulseek' || key === 'usenet' || roster.some((source) => source.key === key)
				)
			: next;
		reorder.mutate(payload, {
			onError: (error: Error) =>
				toastStore.show({
					message: error.message || 'Could not save source priority',
					type: 'error'
				})
		});
	}

	function move(index: number, delta: number) {
		const next = [...order];
		const target = index + delta;
		if (target < 0 || target >= next.length) return;
		[next[index], next[target]] = [next[target], next[index]];
		persist(next);
	}

	function onDrop(targetIndex: number) {
		if (!dragSource) return;
		const next = [...order];
		const from = next.indexOf(dragSource);
		if (from === -1 || from === targetIndex) {
			dragSource = null;
			return;
		}
		next.splice(targetIndex, 0, next.splice(from, 1)[0]);
		dragSource = null;
		persist(next);
	}
</script>

<div class="card border border-base-300 bg-base-200">
	<div class="card-body gap-3">
		<div>
			<h3 class="font-semibold">Source priority</h3>
			<p class="text-sm text-base-content/70">
				Drag (or use ↑/↓) to set which source is tried first for automatic downloads. The top
				enabled source is tried first; the next is the fallback.
			</p>
		</div>

		<ul class="space-y-2">
			{#each order as source, index (source)}
				{@const meta = metaFor(source)}
				{@const Icon = meta.icon}
				<li
					class="flex items-center gap-3 rounded-box border border-base-300 bg-base-100 p-2.5"
					class:opacity-60={meta.missing}
					ondragover={(e) => e.preventDefault()}
					ondrop={() => onDrop(index)}
					role="listitem"
				>
					<button
						type="button"
						class="cursor-grab text-base-content/40 hover:text-base-content"
						aria-label={`Reorder ${meta.label}`}
						draggable="true"
						ondragstart={() => (dragSource = source)}
						ondragend={() => (dragSource = null)}
						onkeydown={(e) => {
							if (e.key === 'ArrowUp') {
								e.preventDefault();
								move(index, -1);
							} else if (e.key === 'ArrowDown') {
								e.preventDefault();
								move(index, 1);
							}
						}}
					>
						<GripVertical class="size-4" aria-hidden="true" />
					</button>
					<span
						class="grid size-8 shrink-0 place-items-center rounded-lg bg-base-300/60 text-base-content/70"
					>
						<Icon class="size-4" aria-hidden="true" />
					</span>
					<span class="badge badge-ghost badge-sm tabular-nums">{index + 1}</span>
					<span class="min-w-0 flex-1">
						<span class="font-medium {meta.missing ? 'text-base-content/50' : ''}"
							>{meta.label}</span
						>
						{#if meta.sub}<span class="text-sm text-base-content/50"> · {meta.sub}</span>{/if}
					</span>
					<div class="flex items-center gap-1">
						<button
							type="button"
							class="btn btn-ghost btn-xs btn-square"
							onclick={() => move(index, -1)}
							disabled={index === 0}
							aria-label={`Move ${meta.label} up`}
						>
							<ChevronUp class="size-4" aria-hidden="true" />
						</button>
						<button
							type="button"
							class="btn btn-ghost btn-xs btn-square"
							onclick={() => move(index, 1)}
							disabled={index === order.length - 1}
							aria-label={`Move ${meta.label} down`}
						>
							<ChevronDown class="size-4" aria-hidden="true" />
						</button>
					</div>
				</li>
			{/each}
		</ul>
	</div>
</div>
