<script lang="ts">
	import { onMount, type Snippet } from 'svelte';
	import { SvelteMap, SvelteSet } from 'svelte/reactivity';
	import {
		ArrowRight,
		ChevronDown,
		ChevronRight,
		FolderCog,
		Image,
		Layers3,
		Tags
	} from 'lucide-svelte';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import type { ManagementAuditDossier, ManagementAuditEntry } from './LibraryManagementAuditTypes';
	import {
		managementReasonLabel,
		type ManagementAuditChangeKind
	} from './LibraryManagementDisplay';

	interface Props {
		dossiers: ManagementAuditDossier[];
		inspector: Snippet<[number]>;
		detailLabel?: string;
		reserveStickyFooter?: boolean;
		stickyFooterHeight?: number;
		incomplete?: boolean;
	}

	let {
		dossiers,
		inspector,
		detailLabel = 'Inspect exact diff',
		reserveStickyFooter = false,
		stickyFooterHeight = 0,
		incomplete = false
	}: Props = $props();
	let onlyExceptions = $state(false);
	let showFullPaths = $state(false);
	let selectedOrdinal = $state<number | null>(null);
	let isWide = $state(false);
	const collapsedBundles = new SvelteSet<number>();

	const visibleDossiers = $derived(
		dossiers
			.map((dossier) => ({
				...dossier,
				entries: onlyExceptions
					? dossier.entries.filter((entry) => entry.exceptional)
					: dossier.entries
			}))
			.filter((dossier) => dossier.entries.length > 0)
	);
	const visibleEntries = $derived(visibleDossiers.flatMap((dossier) => dossier.entries));
	const selectedEntry = $derived(
		visibleEntries.find((entry) => entry.ordinal === selectedOrdinal) ?? visibleEntries[0] ?? null
	);

	onMount(() => {
		const media = window.matchMedia('(min-width: 64rem)');
		const update = () => {
			isWide = media.matches;
		};
		update();
		media.addEventListener('change', update);
		return () => media.removeEventListener('change', update);
	});

	function outcomeStats(dossier: ManagementAuditDossier) {
		const stats = new SvelteMap<
			string,
			{ label: string; count: number; tone: ManagementAuditEntry['statusTone'] }
		>();
		for (const entry of dossier.entries) {
			const key = `${entry.statusTone}:${entry.status}`;
			const existing = stats.get(key);
			if (existing) existing.count += 1;
			else
				stats.set(key, {
					label: entry.status.toLocaleLowerCase(),
					count: 1,
					tone: entry.statusTone
				});
		}
		return Array.from(stats.values());
	}
	function topReason(
		dossier: ManagementAuditDossier
	): { code: string; label: string; count: number } | null {
		const counts = new SvelteMap<string, number>();
		for (const entry of dossier.entries) {
			if (!entry.reasonCode) continue;
			counts.set(entry.reasonCode, (counts.get(entry.reasonCode) ?? 0) + 1);
		}
		let best: { code: string; label: string; count: number } | null = null;
		for (const [code, count] of counts) {
			if (!best || count > best.count) best = { code, label: managementReasonLabel(code), count };
		}
		return best;
	}

	function changeCount(dossier: ManagementAuditDossier, change: ManagementAuditChangeKind) {
		return dossier.entries.filter((entry) => entry.changes.includes(change)).length;
	}

	function originalEntryCount(dossier: ManagementAuditDossier): number {
		return (
			dossiers.find((candidate) => candidate.bundleOrdinal === dossier.bundleOrdinal)?.entries
				.length ?? dossier.entries.length
		);
	}

	function changeLabel(change: ManagementAuditChangeKind): string {
		return change === 'path' ? 'Path' : change.charAt(0).toUpperCase() + change.slice(1);
	}

	function fileName(path: string | null): string {
		return path?.split('/').at(-1) ?? 'No path';
	}

	function pathChanged(entry: ManagementAuditEntry): boolean {
		return Boolean(
			entry.destinationPath &&
			(entry.destinationPath !== entry.sourcePath || entry.destinationRoot !== entry.sourceRoot)
		);
	}

	function displayPath(root: string, path: string | null): string {
		return `${root} · ${path ?? 'No path'}`;
	}

	function toggleBundle(bundleOrdinal: number): void {
		if (collapsedBundles.has(bundleOrdinal)) collapsedBundles.delete(bundleOrdinal);
		else collapsedBundles.add(bundleOrdinal);
	}

	function expandAll(): void {
		collapsedBundles.clear();
	}

	function collapseAll(): void {
		for (const dossier of visibleDossiers) collapsedBundles.add(dossier.bundleOrdinal);
	}
	function focusSelectedTrack(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || !selectedEntry) return;
		event.stopPropagation();
		document.getElementById(`management-track-${selectedEntry.ordinal}`)?.focus();
	}
</script>

<div class="management-audit-toolbar" aria-label="Audit display controls">
	<div class="flex flex-wrap items-center gap-1">
		<button class="btn btn-ghost btn-xs" onclick={expandAll}>Expand releases</button>
		<button class="btn btn-ghost btn-xs" onclick={collapseAll}>Collapse releases</button>
	</div>
	<div class="flex flex-wrap items-center gap-3">
		<label class="management-audit-toggle">
			<input type="checkbox" class="toggle toggle-xs" bind:checked={onlyExceptions} />
			<span>Only exceptions</span>
		</label>
		<label class="management-audit-toggle">
			<input type="checkbox" class="toggle toggle-xs" bind:checked={showFullPaths} />
			<span>Show full paths</span>
		</label>
	</div>
</div>

{#if incomplete}
	<p class="mb-3 rounded-xl bg-info/10 px-3 py-2 text-xs text-base-content/65" role="status">
		More files are available. Dossier counts below describe the files loaded so far.
	</p>
{/if}

{#if visibleDossiers.length === 0}
	<div class="rounded-2xl border border-dashed border-base-content/15 p-8 text-center">
		<strong>No exceptions in the loaded files.</strong>
		<p class="mt-1 text-sm text-base-content/50">
			Turn off “Only exceptions” to review the full plan.
		</p>
	</div>
{:else}
	<div
		class="management-audit-layout"
		data-reserve-sticky-footer={reserveStickyFooter}
		data-testid="management-audit-layout"
		style={`--management-sticky-footer-height: ${stickyFooterHeight}px`}
	>
		<div class="space-y-4">
			{#each visibleDossiers as dossier (dossier.bundleOrdinal)}
				{@const collapsed = collapsedBundles.has(dossier.bundleOrdinal)}
				{@const top = topReason(dossier)}
				<article class="management-dossier" aria-labelledby={`dossier-${dossier.bundleOrdinal}`}>
					<header class="management-dossier-header">
						<div
							class="management-dossier-art"
							aria-hidden="true"
							data-testid="management-dossier-art-frame"
						>
							<AlbumImage
								mbid={dossier.artworkUrl ||
								(dossier.albumId && dossier.albumArtworkVersion !== null)
									? ''
									: (dossier.albumMbid ?? '')}
								albumId={dossier.artworkUrl ? undefined : (dossier.albumId ?? undefined)}
								coverVersion={dossier.artworkUrl
									? undefined
									: (dossier.albumArtworkVersion ?? undefined)}
								customUrl={dossier.artworkUrl}
								alt={`${dossier.title} cover`}
								size="full"
								requestSize={250}
								rounded="none"
								showPlaceholder={true}
								retryOnError={false}
								testId="management-dossier-artwork"
								className="h-full w-full"
							/>
						</div>
						<div class="min-w-0 flex-1">
							<p class="management-step">Release {dossier.bundleOrdinal + 1}</p>
							<h3
								id={`dossier-${dossier.bundleOrdinal}`}
								class="truncate font-display text-lg font-semibold"
							>
								{dossier.title}
							</h3>
							<p class="truncate text-sm text-base-content/55">
								{dossier.artist} · {#if dossier.entries.length < originalEntryCount(dossier)}{dossier.entries.length.toLocaleString()}
									of {originalEntryCount(dossier).toLocaleString()}
								{:else}{dossier.entries.length.toLocaleString()}{/if}
								{incomplete ? 'loaded ' : ''}{originalEntryCount(dossier) === 1 ? 'file' : 'files'}
							</p>
							<div class="management-dossier-stats">
								{#each outcomeStats(dossier) as stat (`${stat.tone}:${stat.label}`)}
									<span data-tone={stat.tone}>{stat.count} {stat.label}</span>
								{/each}
								{#if changeCount(dossier, 'tags')}
									<span><Tags class="h-3 w-3" /> {changeCount(dossier, 'tags')} tags</span>
								{/if}
								{#if changeCount(dossier, 'artwork')}
									<span><Image class="h-3 w-3" /> {changeCount(dossier, 'artwork')} artwork</span>
								{/if}
								{#if changeCount(dossier, 'path')}
									<span><FolderCog class="h-3 w-3" /> {changeCount(dossier, 'path')} path</span>
								{/if}
								{#if changeCount(dossier, 'sidecars')}
									<span
										><Layers3 class="h-3 w-3" /> {changeCount(dossier, 'sidecars')} sidecars</span
									>
								{/if}
							</div>
							{#if top}
								<p class="mt-1 text-xs text-base-content/55">
									{#if top.code === 'BUNDLE_BLOCKED'}
										Held by a blocked file in this release (Release {dossier.bundleOrdinal + 1}) · {top.label}
										({top.count.toLocaleString()})
									{:else}
										Top reason: {top.label} ({top.count.toLocaleString()})
									{/if}
								</p>
							{/if}
						</div>
						<button
							class="btn btn-ghost btn-sm btn-square shrink-0"
							aria-label={`${collapsed ? 'Expand' : 'Collapse'} ${dossier.title}`}
							aria-expanded={!collapsed}
							onclick={() => toggleBundle(dossier.bundleOrdinal)}
						>
							{#if collapsed}
								<ChevronRight class="h-4 w-4" />
							{:else}
								<ChevronDown class="h-4 w-4" />
							{/if}
						</button>
					</header>

					{#if !collapsed}
						<div class="management-dossier-tracks">
							{#each dossier.entries as entry (entry.ordinal)}
								{@const active = selectedEntry?.ordinal === entry.ordinal}
								<button
									id={`management-track-${entry.ordinal}`}
									class="management-audit-track"
									data-selected={active}
									data-tone={entry.statusTone}
									aria-pressed={active}
									aria-expanded={active}
									aria-controls="management-inspector"
									aria-label={`${detailLabel} for ${entry.title}`}
									onclick={() => (selectedOrdinal = entry.ordinal)}
								>
									<span class="management-audit-track-number">{entry.trackLabel}</span>
									<span class="badge badge-ghost badge-sm font-mono"
										>{entry.format.toUpperCase()}</span
									>
									<span class="min-w-0 flex-1 text-left">
										<strong class="block truncate text-sm">{entry.title}</strong>
										{#if entry.artist && entry.artist !== dossier.artist}
											<small class="block truncate text-base-content/50">{entry.artist}</small>
										{/if}
										{#if entry.reason && active}
											<small class="block truncate font-semibold text-error">{entry.reason}</small>
										{/if}
										{#if pathChanged(entry)}
											<span
												class="management-audit-path"
												aria-label={`File path changed from ${entry.sourcePath} to ${entry.destinationPath}`}
											>
												{showFullPaths
													? displayPath(entry.sourceRoot, entry.sourcePath)
													: fileName(entry.sourcePath)}
												<ArrowRight class="h-3 w-3 shrink-0" />
												<strong
													>{showFullPaths
														? displayPath(entry.destinationRoot, entry.destinationPath)
														: fileName(entry.destinationPath)}</strong
												>
											</span>
										{:else if showFullPaths}
											<span class="management-audit-path"
												>{displayPath(entry.sourceRoot, entry.sourcePath)}</span
											>
										{/if}
									</span>
									<span class="management-audit-changes">
										{#each entry.changes as change (change)}
											<span class="management-change-chip" title={changeLabel(change)}>
												{#if change === 'tags'}<Tags
														class="h-3 w-3"
													/>{:else if change === 'artwork'}<Image
														class="h-3 w-3"
													/>{:else if change === 'path'}<FolderCog class="h-3 w-3" />{:else}<Layers3
														class="h-3 w-3"
													/>{/if}
												<span>{changeLabel(change)}</span>
											</span>
										{/each}
									</span>
									<span
										class="badge badge-sm {entry.statusTone === 'success'
											? 'badge-success'
											: entry.statusTone === 'warning'
												? 'badge-warning'
												: entry.statusTone === 'error'
													? 'badge-error'
													: 'badge-outline'}">{entry.status}</span
									>
									<ChevronRight class="h-4 w-4 shrink-0 text-base-content/35" />
								</button>
								{#if !isWide && active}
									<div
										id="management-inspector"
										class="management-audit-inline-inspector"
										role="region"
										aria-label={`${detailLabel}: ${entry.title}`}
										aria-live="polite"
										data-testid="management-audit-inspector"
										onkeydown={focusSelectedTrack}
									>
										{@render inspector(entry.ordinal)}
									</div>
								{/if}
							{/each}
						</div>
					{/if}
				</article>
			{/each}
		</div>

		{#if isWide && selectedEntry}
			<aside
				id="management-inspector"
				class="management-audit-inspector"
				aria-label={`${detailLabel}: ${selectedEntry.title}`}
				aria-live="polite"
				data-testid="management-audit-inspector"
				onkeydown={focusSelectedTrack}
			>
				{@render inspector(selectedEntry.ordinal)}
			</aside>
		{/if}
	</div>
{/if}
