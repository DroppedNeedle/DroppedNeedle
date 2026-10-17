<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { onMount, type Snippet } from 'svelte';
	import { SvelteMap, SvelteSet } from 'svelte/reactivity';
	import {
		ArrowRight,
		ChevronDown,
		ChevronRight,
		FolderCog,
		Image,
		Layers,
		Tags
	} from 'lucide-svelte';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
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
		<Button variant="ghost" size="xs" onclick={expandAll}>Expand releases</Button>
		<Button variant="ghost" size="xs" onclick={collapseAll}>Collapse releases</Button>
	</div>
	<div class="flex flex-wrap items-center gap-3">
		<label class="management-audit-toggle">
			<input
				type="checkbox"
				class="toggle toggle-sm toggle-primary"
				bind:checked={onlyExceptions}
			/>
			<span>Only exceptions</span>
		</label>
		<label class="management-audit-toggle">
			<input type="checkbox" class="toggle toggle-sm toggle-primary" bind:checked={showFullPaths} />
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
								albumId={dossier.artworkUrl || dossier.albumArtworkVersion === null
									? undefined
									: (dossier.albumId ?? undefined)}
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
									<span><Layers class="h-3 w-3" /> {changeCount(dossier, 'sidecars')} sidecars</span
									>
								{/if}
							</div>
							{#if top}
								<p class="mt-1 text-xs text-base-content/55">
									{#if top.code === 'BUNDLE_BLOCKED'}
										Held by a blocked file in this release (Release {dossier.bundleOrdinal + 1}) · {top.label}
										({top.count.toLocaleString()})
									{:else}
										Top reason: {top.code === 'OPTIONAL_ENRICHMENT_DEFERRED'
											? 'Optional enrichment deferred · still applicable with warnings'
											: top.label} ({top.count.toLocaleString()})
									{/if}
								</p>
							{/if}
						</div>
						<IconButton
							size="sm"
							shape="square"
							class="shrink-0"
							icon={collapsed ? ChevronRight : ChevronDown}
							label={`${collapsed ? 'Expand' : 'Collapse'} ${dossier.title}`}
							aria-expanded={!collapsed}
							onclick={() => toggleBundle(dossier.bundleOrdinal)}
						/>
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
													/>{:else if change === 'path'}<FolderCog class="h-3 w-3" />{:else}<Layers
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

<style>
	.management-audit-toolbar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.6rem 0.75rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 9%, transparent);
		border-radius: 0.85rem;
		background: color-mix(in oklab, var(--color-base-200) 55%, transparent);
	}

	.management-audit-toggle {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		font-size: 0.75rem;
		font-weight: 650;
		cursor: pointer;
	}

	.management-audit-layout {
		display: grid;
		gap: 1rem;
	}

	.management-dossier {
		overflow: clip;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 11%, transparent);
		border-radius: 1rem;
		background: color-mix(in oklab, var(--color-base-100) 94%, transparent);
		box-shadow: 0 0.5rem 1.4rem color-mix(in oklab, var(--color-base-content) 4%, transparent);
	}

	.management-dossier-header {
		display: flex;
		align-items: center;
		gap: 0.85rem;
		padding: 0.85rem;
		border-bottom: 1px solid color-mix(in oklab, var(--color-base-content) 9%, transparent);
		background: linear-gradient(
			105deg,
			color-mix(in oklab, var(--color-library-manage) 9%, var(--color-base-100)),
			var(--color-base-100) 68%
		);
	}

	.management-dossier-art {
		position: relative;
		display: block;
		width: 3.75rem;
		height: 3.75rem;
		flex: 0 0 auto;
		overflow: hidden;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent);
		border-radius: 0.75rem;
		background: color-mix(in oklab, var(--color-base-200) 78%, var(--color-base-100));
	}

	.management-dossier-art > :global(div) {
		width: 100%;
		height: 100%;
	}

	.management-dossier-art :global(img) {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	.management-dossier-stats {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
		margin-top: 0.45rem;
	}

	.management-dossier-stats span {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.15rem 0.4rem;
		border-radius: 999px;
		background: color-mix(in oklab, var(--color-base-content) 6%, transparent);
		color: color-mix(in oklab, var(--color-base-content) 63%, transparent);
		font-size: 0.66rem;
		font-weight: 700;
	}

	.management-dossier-stats span[data-tone='success'] {
		background: color-mix(in oklab, var(--color-success) 11%, transparent);
		color: color-mix(in oklab, var(--color-success) 72%, var(--color-base-content));
	}

	.management-dossier-stats span[data-tone='warning'] {
		background: color-mix(in oklab, var(--color-warning) 13%, transparent);
		color: color-mix(in oklab, var(--color-warning) 72%, var(--color-base-content));
	}

	.management-dossier-stats span[data-tone='error'] {
		background: color-mix(in oklab, var(--color-error) 11%, transparent);
		color: color-mix(in oklab, var(--color-error) 72%, var(--color-base-content));
	}

	.management-dossier-tracks {
		display: grid;
	}

	.management-audit-track {
		position: relative;
		display: grid;
		grid-template-columns: 2.5rem auto minmax(0, 1fr) auto auto 1rem;
		align-items: center;
		gap: 0.6rem;
		width: 100%;
		min-height: 3.75rem;
		padding: 0.55rem 0.75rem;
		border-top: 1px solid color-mix(in oklab, var(--color-base-content) 7%, transparent);
		text-align: left;
		cursor: pointer;
	}

	.management-audit-track:first-child {
		border-top: 0;
	}

	.management-audit-track::before {
		position: absolute;
		inset-block: 0.45rem;
		left: 0;
		width: 0.2rem;
		border-radius: 999px;
		background: transparent;
		content: '';
	}

	.management-audit-track:hover {
		background: color-mix(in oklab, var(--color-library-manage) 5%, transparent);
	}

	.management-audit-track:focus-visible {
		z-index: 1;
		outline: 2px solid var(--color-library-manage);
		outline-offset: -2px;
	}

	.management-audit-track[data-selected='true'] {
		background: color-mix(in oklab, var(--color-library-manage) 9%, transparent);
	}

	.management-audit-track[data-selected='true']::before {
		background: var(--color-library-manage);
	}

	.management-audit-track[data-tone='error']::before {
		background: var(--color-error);
	}

	.management-audit-track[data-tone='warning']:not([data-selected='true'])::before {
		background: var(--color-warning);
	}

	.management-audit-track-number {
		color: color-mix(in oklab, var(--color-base-content) 45%, transparent);
		font-family: var(--font-mono);
		font-size: 0.72rem;
		font-variant-numeric: tabular-nums;
		text-align: right;
	}

	.management-audit-path {
		display: flex;
		align-items: center;
		gap: 0.3rem;
		min-width: 0;
		margin-top: 0.15rem;
		overflow-wrap: anywhere;
		color: color-mix(in oklab, var(--color-base-content) 50%, transparent);
		font-family: var(--font-mono);
		font-size: 0.66rem;
	}

	.management-audit-changes {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.25rem;
	}

	.management-change-chip {
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		padding: 0.18rem 0.36rem;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 18%, transparent);
		border-radius: 999px;
		background: color-mix(in oklab, var(--color-library-manage) 6%, transparent);
		color: color-mix(in oklab, var(--color-library-manage) 72%, var(--color-base-content));
		font-size: 0.64rem;
		font-weight: 700;
	}

	.management-audit-inline-inspector,
	.management-audit-inspector {
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 20%, transparent);
		background: color-mix(in oklab, var(--color-base-100) 97%, transparent);
	}

	.management-audit-inline-inspector {
		padding: 0.85rem;
		border-inline: 0;
		border-bottom: 0;
	}

	@media (min-width: 1024px) {
		.management-audit-layout {
			grid-template-columns: minmax(0, 1.12fr) minmax(22rem, 0.88fr);
			align-items: start;
		}

		.management-audit-inspector {
			position: sticky;
			top: calc(
				var(--droppedneedle-topbar-height, 4rem) +
					var(--droppedneedle-library-activity-height, 0px) + 1rem
			);
			max-height: calc(
				100dvh - var(--droppedneedle-topbar-height, 4rem) -
					var(--droppedneedle-library-activity-height, 0px) - 2rem
			);
			overflow-y: auto;
			overscroll-behavior: contain;
			padding: 1rem;
			border-radius: 1rem;
			scrollbar-gutter: stable;
		}

		.management-audit-layout[data-reserve-sticky-footer='true'] .management-audit-inspector {
			max-height: calc(
				100dvh - var(--droppedneedle-topbar-height, 4rem) -
					var(--droppedneedle-library-activity-height, 0px) -
					var(--management-sticky-footer-height, 0px) - 2.75rem
			);
		}
	}

	@media (max-width: 639px) {
		.management-audit-toolbar {
			align-items: stretch;
			flex-direction: column;
		}

		.management-audit-track {
			grid-template-columns: 2rem auto minmax(0, 1fr) 1rem;
			gap: 0.45rem;
			padding-inline: 0.55rem;
		}

		.management-audit-changes,
		.management-audit-track > .badge:last-of-type {
			grid-column: 3;
			justify-content: flex-start;
		}

		.management-audit-track > :global(svg:last-child) {
			grid-column: 4;
			grid-row: 1;
		}

		.management-change-chip span {
			display: none;
		}
	}
</style>
