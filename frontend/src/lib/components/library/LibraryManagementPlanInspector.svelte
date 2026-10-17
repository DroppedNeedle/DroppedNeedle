<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { ArrowRight, Eraser, FileClock, Image, RotateCcw, Tags } from 'lucide-svelte';

	import { getApiUrl } from '$lib/api/api-utils';
	import { LIBRARY_MANAGEMENT_ENDPOINTS } from '$lib/queries/library-management/endpoints';
	import type { LibraryManagementPlanItem } from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import {
		firstDeferredSource,
		formatManagementBytes,
		formatManagementValue,
		managementAdapter,
		managementAudioFormat,
		managementArtworkPreviewHash,
		managementCollisions,
		managementCustomTagDiffs,
		managementDisplayPath,
		managementFieldDiffs,
		managementPlanAlbum,
		managementPlanArtist,
		managementPlanTitle,
		managementRestoration,
		managementScrubbedRawTags,
		managementSidecars,
		managementStringList,
		titleManagementValue,
		type ManagementCollision,
		type ManagementRestorationArtwork
	} from './LibraryManagementDisplay';
	import LibraryManagementLyricsEvidence from './LibraryManagementLyricsEvidence.svelte';

	interface Props {
		item: LibraryManagementPlanItem;
		jobId: string;
		roots: Array<{ id: string; label: string }>;
		reasonLabel: (value: string) => string;
		onresolve: (
			item: LibraryManagementPlanItem,
			collision: ManagementCollision,
			opener: HTMLElement
		) => void;
	}

	let { item, jobId, roots, reasonLabel, onresolve }: Props = $props();
	const diffs = $derived([...managementFieldDiffs(item), ...managementCustomTagDiffs(item)]);
	const restoration = $derived(managementRestoration(item));
	const scrubbedRawTags = $derived(managementScrubbedRawTags(item));
	const warnings = $derived(managementStringList(item.capability.warnings));
	const blockers = $derived(managementStringList(item.capability.blockers));
	const losses = $derived(managementStringList(item.capability.representation_losses));
	const sidecars = $derived(managementSidecars(item));
	const collisions = $derived(managementCollisions(item));
	const title = $derived(managementPlanTitle(item));
	const artist = $derived(managementPlanArtist(item));
	const album = $derived(managementPlanAlbum(item));
	const deferredSource = $derived(firstDeferredSource(item));

	function displayPath(root: string | null, relative: string | null): string {
		return managementDisplayPath(roots, root, relative);
	}

	function formatNanosecondDate(value: string | null): string {
		if (!value) return 'Unavailable';
		const milliseconds = Number(value) / 1_000_000;
		return Number.isFinite(milliseconds) ? new Date(milliseconds).toLocaleString() : 'Unavailable';
	}

	function formatPermissions(value: number | null): string {
		return value === null ? 'Unavailable' : `0${value.toString(8).padStart(3, '0')}`;
	}

	function restorationScopeLabel(value: string): string {
		return value === 'first_management_baseline' ? 'Original baseline' : 'Operation before-state';
	}

	function artworkSnapshotDescription(value: ManagementRestorationArtwork): string {
		return [
			titleManagementValue(value.imageType),
			value.width !== null && value.height !== null
				? `${value.width.toLocaleString()} × ${value.height.toLocaleString()} px`
				: null,
			value.mimeType,
			formatManagementBytes(value.byteSize)
		]
			.filter(Boolean)
			.join(' · ');
	}

	function shortFingerprint(value: string | null): string {
		return value ? `${value.slice(0, 12)}…${value.slice(-8)}` : 'Unavailable';
	}

	function artworkText(choice: Record<string, unknown>, key: string): string | null {
		const value = choice[key];
		return typeof value === 'string' && value.trim() ? value : null;
	}

	function artworkDimensions(choice: Record<string, unknown>): string | null {
		const width = choice.width;
		const height = choice.height;
		return typeof width === 'number' && typeof height === 'number'
			? `${width.toLocaleString()} × ${height.toLocaleString()} px`
			: null;
	}

	function artworkPreviewUrl(choice: Record<string, unknown>): string | null {
		const sha256 = managementArtworkPreviewHash(choice);
		if (!sha256) return null;
		return getApiUrl(LIBRARY_MANAGEMENT_ENDPOINTS.previewArtwork(jobId, item.ordinal, sha256));
	}
</script>

<div class="management-inspector-content">
	<header class="management-inspector-heading">
		<div class="min-w-0 flex-1">
			<p class="management-step">Inspect exact diff</p>
			<h3 class="mt-1 font-display text-lg font-semibold">{title}</h3>
			<p class="mt-1 text-sm text-base-content/55">
				{[artist, album].filter(Boolean).join(' · ') || `Release ${item.bundle_ordinal + 1}`}
			</p>
		</div>
		<div class="flex flex-wrap items-center justify-end gap-1">
			<span class="badge badge-ghost badge-sm font-mono"
				>{managementAudioFormat(item).toUpperCase()}</span
			>
			<span
				class="badge badge-sm {item.eligibility === 'eligible'
					? 'badge-success'
					: item.eligibility === 'warning'
						? 'badge-warning'
						: 'badge-error'}">{titleManagementValue(item.eligibility)}</span
			>
		</div>
	</header>

	<div class="management-inspector-paths">
		<div>
			<small>Current</small>
			<code>{displayPath(item.source_root_id, item.source_relative_path)}</code>
		</div>
		{#if item.destination_relative_path && (item.destination_root_id !== item.source_root_id || item.destination_relative_path !== item.source_relative_path)}
			<ArrowRight class="h-4 w-4 shrink-0 text-library-manage" />
			<div>
				<small>Planned</small>
				<code>{displayPath(item.destination_root_id, item.destination_relative_path)}</code>
			</div>
		{/if}
	</div>

	{#if item.reason_code}
		<div
			class="rounded-lg border p-2 text-sm {item.eligibility === 'warning'
				? 'border-warning/25 bg-warning/5'
				: 'border-error/20 bg-error/5'}"
		>
			<p class="font-semibold {item.eligibility === 'warning' ? 'text-warning' : 'text-error'}">
				{reasonLabel(item.reason_code)}{item.reason_code === 'OPTIONAL_ENRICHMENT_DEFERRED' &&
				deferredSource
					? ` (${deferredSource})`
					: ''}
			</p>
			{#if item.reason?.message}
				<p class="mt-1 text-base-content/75">{item.reason.message}</p>
			{/if}
			{#if item.reason?.action}
				<p class="mt-1 text-base-content/60">
					<span class="font-semibold">What to do:</span>
					{item.reason.action}
				</p>
			{/if}
		</div>
	{/if}

	<LibraryManagementLyricsEvidence {item} />

	{#if scrubbedRawTags.length}
		<section class="rounded-xl border border-warning/30 bg-warning/5 p-3">
			<div class="flex items-start justify-between gap-3">
				<div class="flex items-start gap-2">
					<Eraser class="mt-0.5 h-4 w-4 shrink-0 text-warning" />
					<div>
						<h4 class="management-inspector-section-title">Unmanaged tags to remove</h4>
						<p class="mt-1 text-xs text-base-content/55">
							Explicit scrub will delete these native entries because they are not in the profile's
							preserve list.
						</p>
					</div>
				</div>
				<span class="badge badge-warning badge-sm">{scrubbedRawTags.length}</span>
			</div>
			<div
				class="mt-3 divide-y divide-warning/15 rounded-lg border border-warning/15 bg-base-100/60"
			>
				{#each scrubbedRawTags as tag (`${tag.key}:${tag.sha256 ?? ''}`)}
					<div class="grid gap-2 p-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-start">
						<div class="min-w-0">
							<code class="break-all text-xs font-semibold">{tag.key}</code>
							<p class="mt-1 break-words text-xs text-base-content/60">
								{tag.valueKind === 'binary'
									? 'Binary metadata'
									: tag.values.join(' · ') || 'Empty value'}
							</p>
							{#if tag.truncated}
								<small class="mt-1 block text-base-content/45">
									Preview shortened · {tag.valueCount.toLocaleString()} values · {shortFingerprint(
										tag.sha256
									)}
								</small>
							{:else if tag.valueKind === 'binary'}
								<small class="mt-1 block font-mono text-base-content/45">
									{shortFingerprint(tag.sha256)}
								</small>
							{/if}
						</div>
						<span class="badge badge-warning badge-outline badge-sm">Remove</span>
					</div>
				{/each}
			</div>
		</section>
	{/if}

	{#if diffs.length}
		<section>
			<h4 class="management-inspector-section-title">Metadata</h4>
			<div class="divide-y divide-base-content/10">
				{#each diffs as diff (`${diff.name}:${diff.operation}`)}
					<div class="management-diff-row">
						<strong class="text-sm">{titleManagementValue(diff.name)}</strong>
						<span class="management-diff-value" data-side="before"
							>{formatManagementValue(diff.before)}</span
						>
						<span
							class="management-diff-badge text-xs font-bold uppercase"
							data-operation={diff.operation}
						>
							{titleManagementValue(diff.operation)}
							<ArrowRight class="inline h-3 w-3" />
						</span>
						<span class="management-diff-value" data-side="after"
							>{formatManagementValue(diff.after)}</span
						>
						{#if diff.representationLoss}
							<small class="text-warning sm:col-span-4"
								>Format representation: {diff.representationLoss}</small
							>
						{/if}
					</div>
				{/each}
			</div>
		</section>
	{/if}

	{#if restoration}
		<section class="rounded-xl border border-library-manage/20 bg-library-manage/5 p-3">
			<div class="flex flex-wrap items-start justify-between gap-2">
				<div class="flex items-start gap-2">
					<RotateCcw class="mt-0.5 h-4 w-4 shrink-0 text-library-manage" />
					<div>
						<h4 class="management-inspector-section-title">Sealed restoration snapshot</h4>
						<p class="mt-1 text-xs text-base-content/55">
							The staged writer restores this pinned native state, then verifies it before
							publishing.
						</p>
					</div>
				</div>
				<span class="badge badge-outline badge-sm">{restorationScopeLabel(restoration.scope)}</span>
			</div>

			<div class="mt-3 grid gap-3 xl:grid-cols-2">
				<Card as="article" variant="bordered" padding="sm">
					<div class="flex items-center gap-2">
						<Tags class="h-4 w-4 text-library-manage" />
						<strong class="text-sm">Native tag payload</strong>
						<span
							class="badge badge-sm {restoration.nativeTags.changed
								? 'badge-warning'
								: 'badge-ghost'}"
						>
							{restoration.nativeTags.changed ? 'Restored' : 'Unchanged'}
						</span>
					</div>
					<p class="mt-2 text-xs text-base-content/55">
						Primary entries {restoration.nativeTags.currentPrimaryEntries.toLocaleString()} →
						{restoration.nativeTags.restoredPrimaryEntries.toLocaleString()} · auxiliary
						{restoration.nativeTags.currentAuxiliaryEntries.toLocaleString()} →
						{restoration.nativeTags.restoredAuxiliaryEntries.toLocaleString()}
					</p>
					{#if restoration.nativeTags.changedRawKeys.length}
						<p class="mt-2 break-words text-xs">
							<strong>Native keys:</strong>
							{restoration.nativeTags.changedRawKeys.join(' · ')}
						</p>
					{/if}
					<code class="mt-2 block break-all text-[0.65rem] text-base-content/45">
						{shortFingerprint(restoration.nativeTags.currentFingerprint)} →
						{shortFingerprint(restoration.nativeTags.restoredFingerprint)}
					</code>
				</Card>

				<Card as="article" variant="bordered" padding="sm">
					<div class="flex items-center gap-2">
						<FileClock class="h-4 w-4 text-library-manage" />
						<strong class="text-sm">File attributes</strong>
						<span
							class="badge badge-sm {restoration.fileAttributes.changed
								? 'badge-warning'
								: 'badge-ghost'}"
						>
							{restoration.fileAttributes.changed ? 'Restored' : 'Unchanged'}
						</span>
					</div>
					<dl class="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
						<dt class="text-base-content/45">Modified</dt>
						<dd>
							{formatNanosecondDate(restoration.fileAttributes.currentMtimeNs)} → {formatNanosecondDate(
								restoration.fileAttributes.restoredMtimeNs
							)}
						</dd>
						<dt class="text-base-content/45">Permissions</dt>
						<dd class="font-mono">
							{formatPermissions(restoration.fileAttributes.currentPermissionBits)} → {formatPermissions(
								restoration.fileAttributes.restoredPermissionBits
							)}
						</dd>
					</dl>
				</Card>
			</div>

			{#if restoration.artwork.changed}
				<div class="mt-3">
					<div class="flex items-center gap-2">
						<Image class="h-4 w-4 text-library-manage" />
						<strong class="text-sm">Embedded artwork</strong>
					</div>
					<div class="mt-2 grid gap-2 sm:grid-cols-[1fr_auto_1fr]">
						<div class="management-diff-value" data-side="before">
							<small class="font-bold uppercase text-base-content/45">Current</small>
							{#if restoration.artwork.current.length}
								<ul class="mt-1 space-y-1">
									{#each restoration.artwork.current as artwork (`current:${artwork.sha256}`)}
										<li class="text-xs">
											{artworkSnapshotDescription(artwork)}<code
												class="mt-0.5 block text-[0.65rem] text-base-content/45"
												>{shortFingerprint(artwork.sha256)}</code
											>
										</li>
									{/each}
								</ul>
							{:else}<p class="mt-1 text-xs">No embedded artwork</p>{/if}
						</div>
						<ArrowRight class="mt-3 h-4 w-4" />
						<div class="management-diff-value" data-side="after">
							<small class="font-bold uppercase text-base-content/45">Restore</small>
							{#if restoration.artwork.restored.length}
								<ul class="mt-1 space-y-1">
									{#each restoration.artwork.restored as artwork (`restored:${artwork.sha256}`)}
										<li class="text-xs">
											{artworkSnapshotDescription(artwork)}<code
												class="mt-0.5 block text-[0.65rem] text-base-content/45"
												>{shortFingerprint(artwork.sha256)}</code
											>
										</li>
									{/each}
								</ul>
							{:else}<p class="mt-1 text-xs">No embedded artwork</p>{/if}
						</div>
					</div>
				</div>
			{/if}
		</section>
	{/if}

	{#if item.destination_relative_path && (item.source_relative_path !== item.destination_relative_path || item.source_root_id !== item.destination_root_id)}
		<section>
			<h4 class="management-inspector-section-title">Organization</h4>
			<div class="grid gap-2 sm:grid-cols-[1fr_auto_1fr]">
				<code class="management-diff-value" data-side="before"
					>{displayPath(item.source_root_id, item.source_relative_path)}</code
				>
				<ArrowRight class="mt-2 h-4 w-4" />
				<code class="management-diff-value" data-side="after"
					>{displayPath(item.destination_root_id, item.destination_relative_path)}</code
				>
			</div>
		</section>
	{/if}

	{#if warnings.length || blockers.length || losses.length}
		<section class="grid gap-3 sm:grid-cols-3">
			<div>
				<h4 class="management-inspector-section-title">Adapter</h4>
				<p class="text-sm">
					{managementAdapter(item) ?? 'No writer adapter'} · {managementAudioFormat(
						item
					).toUpperCase()}
				</p>
			</div>
			<div>
				<h4 class="management-inspector-section-title">Warnings</h4>
				<p class="text-sm">
					{[...warnings, ...losses].map(titleManagementValue).join(' · ') || 'None'}
				</p>
			</div>
			<div>
				<h4 class="management-inspector-section-title">Blockers</h4>
				<p class="text-sm">{blockers.map(titleManagementValue).join(' · ') || 'None'}</p>
			</div>
		</section>
	{/if}

	{#if sidecars.length}
		<section>
			<h4 class="management-inspector-section-title">Sidecars</h4>
			<ul class="mt-1 space-y-1 text-sm">
				{#each sidecars as sidecar, index (index)}<li>{formatManagementValue(sidecar)}</li>{/each}
			</ul>
		</section>
	{/if}

	{#if item.artwork_choices.length}
		<section>
			<h4 class="management-inspector-section-title">Artwork</h4>
			<div class="mt-2 grid gap-2 xl:grid-cols-2">
				{#each item.artwork_choices as choice, index (index)}
					<Card as="article" variant="bordered" padding="sm" class="text-xs">
						<div class="flex gap-3">
							{#if artworkPreviewUrl(choice)}
								<img
									src={artworkPreviewUrl(choice)}
									alt={`${titleManagementValue(artworkText(choice, 'image_type') ?? 'Artwork')} preview`}
									class="h-20 w-20 shrink-0 rounded-md bg-base-200 object-cover"
									loading="lazy"
									decoding="async"
								/>
							{/if}
							<div class="min-w-0">
								<strong
									>{titleManagementValue(
										artworkText(choice, 'output_kind') ?? 'artwork'
									)}{artworkText(choice, 'image_type')
										? ` · ${titleManagementValue(artworkText(choice, 'image_type') ?? '')}`
										: ''}</strong
								>
								<p class="mt-1 text-base-content/55">
									{[
										artworkText(choice, 'source')
											? titleManagementValue(artworkText(choice, 'source') ?? '')
											: null,
										artworkDimensions(choice),
										artworkText(choice, 'format')?.toUpperCase(),
										artworkText(choice, 'mime_type')
									]
										.filter(Boolean)
										.join(' · ') || 'Pinned output details unavailable'}
								</p>
								{#if artworkText(choice, 'destination_relative_path')}<code
										class="mt-1 block break-all text-base-content/45"
										>{artworkText(choice, 'destination_relative_path')}</code
									>{/if}
							</div>
						</div>
					</Card>
				{/each}
			</div>
		</section>
	{/if}

	{#if collisions.length}
		<section class="space-y-2">
			<h4 class="management-inspector-section-title text-error">Collision evidence</h4>
			{#each collisions as collision, index (index)}
				<div class="rounded-xl border border-error/25 bg-error/5 p-3">
					<strong class="text-sm">{titleManagementValue(collision.classification)}</strong>
					<p class="mt-1 text-xs text-base-content/55">
						No file is assumed safe to delete. Both sides are revalidated in a new preview.
					</p>
					{#if collision.requestKind && collision.existingRootId && collision.existingRelativePath}
						<Button
							variant="outline"
							size="sm"
							class="mt-3"
							onclick={(event) => onresolve(item, collision, event.currentTarget)}
							>Choose resolution...</Button
						>
					{:else}
						<span class="badge badge-error badge-outline mt-3">Requires fresh scan evidence</span>
					{/if}
				</div>
			{/each}
		</section>
	{/if}
</div>

<style>
	.management-diff-row {
		display: grid;
		grid-template-columns: minmax(7rem, 0.7fr) minmax(0, 1fr) 7.25rem minmax(0, 1fr);
		gap: 0.65rem;
		align-items: start;
		padding: 0.65rem 0;
		border-top: 1px solid color-mix(in oklab, var(--color-base-content) 8%, transparent);
	}

	.management-diff-value {
		min-width: 0;
		padding: 0.4rem 0.55rem;
		border-radius: 0.55rem;
		overflow-wrap: anywhere;
		font-family: var(--font-mono);
		font-size: 0.73rem;
	}

	.management-diff-value[data-side='before'] {
		background: color-mix(in oklab, var(--color-management-removed) 8%, transparent);
	}

	.management-diff-value[data-side='after'] {
		background: color-mix(in oklab, var(--color-management-added) 8%, transparent);
	}

	.management-diff-badge[data-operation='preserve'],
	.management-diff-badge[data-operation='unchanged'] {
		color: var(--color-management-preserved);
	}

	.management-diff-badge {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.3rem;
		padding: 0.4rem 0.5rem;
	}

	@media (max-width: 639px) {
		.management-diff-row {
			grid-template-columns: 1fr;
		}
	}

	/* the narrower diff beside the audit list */
	@media (min-width: 1024px) {
		:global(.management-audit-inspector) .management-diff-row {
			grid-template-columns: minmax(6rem, 0.55fr) minmax(0, 1fr);
		}

		:global(.management-audit-inspector) .management-diff-row > :nth-child(3) {
			grid-column: 1;
		}

		:global(.management-audit-inspector) .management-diff-row > small {
			grid-column: 1 / -1;
		}
	}
</style>
