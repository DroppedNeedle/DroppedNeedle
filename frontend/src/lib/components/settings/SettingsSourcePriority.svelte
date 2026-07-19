<script lang="ts">
	import {
		ChevronDown,
		ChevronUp,
		GripVertical,
		HardDriveDownload,
		Magnet,
		Rss
	} from 'lucide-svelte';

	import {
		getSourcePriorityQuery,
		saveSourcePriority
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';

	const priorityQuery = getSourcePriorityQuery();
	const reorder = saveSourcePriority();

	const META: Record<string, { label: string; sub: string; icon: typeof Rss }> = {
		soulseek: { label: 'Soulseek', sub: 'slskd', icon: HardDriveDownload },
		usenet: { label: 'Usenet', sub: 'SABnzbd', icon: Rss },
		torrent: { label: 'Torrents', sub: 'Torrent client via Prowlarr', icon: Magnet }
	};

	const order = $derived(priorityQuery.data?.order ?? ['soulseek', 'usenet', 'torrent']);
	let dragSource = $state<string | null>(null);

	function persist(next: string[]) {
		reorder.mutate(next);
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
				Drag (or use ↑/↓) to set which source is tried first for automatic downloads. The topmost
				enabled source gets first shot; the next is the fallback.
			</p>
		</div>

		<ul class="space-y-2">
			{#each order as source, index (source)}
				{@const meta = META[source] ?? { label: source, sub: '', icon: HardDriveDownload }}
				{@const Icon = meta.icon}
				<li
					class="flex items-center gap-3 rounded-box border border-base-300 bg-base-100 p-2.5"
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
						<span class="font-medium">{meta.label}</span>
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
