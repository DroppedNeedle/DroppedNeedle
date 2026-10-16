<script lang="ts">
	import { ExternalLink, RefreshCw, Search, Unlink } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import type { LibraryContribution } from '$lib/types';
	import {
		removeDiscogsReleaseMutation,
		searchDiscogsReleasesMutation,
		selectDiscogsReleaseMutation
	} from '$lib/queries/libraryContributions/LibraryContributionMutations.svelte';

	interface Props {
		contribution: LibraryContribution;
		canMutate: boolean;
	}

	let { contribution, canMutate }: Props = $props();
	let query = $state('');
	const searchMutation = searchDiscogsReleasesMutation();
	const selectMutation = selectDiscogsReleaseMutation();
	const removeMutation = removeDiscogsReleaseMutation();
	const selectedReference = $derived(
		contribution.source_selection.sources.find(
			(source) => source.provider === 'discogs' && source.entity_type === 'release'
		)
	);
	const selected = $derived(contribution.discogs_source?.release ?? null);
	const exactInput = $derived(/^\d+$/.test(query.trim()) || query.includes('discogs.com/release/'));

	function search(): void {
		if (!canMutate || searchMutation.isPending) return;
		const value = query.trim();
		if (
			/^\d+$/.test(value) ||
			/^https:\/\/(?:www\.)?discogs\.com\/release\/[1-9]\d*(?:-[^/?#]+)?\/?$/i.test(value)
		) {
			select(value);
			return;
		}
		searchMutation.mutate({ contributionId: contribution.id, query });
	}

	function select(releaseIdOrUrl: string): void {
		if (!canMutate || selectMutation.isPending) return;
		selectMutation.mutate({
			contributionId: contribution.id,
			expectedRowRevision: contribution.row_revision,
			releaseIdOrUrl
		});
	}
</script>

<Card as="section" variant="bordered" padding="none" aria-label="Find the exact Discogs release">
	<div class="border-b border-base-content/10 px-5 py-4 sm:px-6">
		<p class="text-xs font-bold uppercase tracking-[0.16em] text-base-content/40">
			Optional source
		</p>
		<h2 class="mt-1 text-lg font-bold">Find the exact Discogs release</h2>
		<p class="mt-1 text-sm text-base-content/55">
			Use a release URL or ID, or search by artist, title, barcode, label, or catalogue number.
		</p>
	</div>

	{#if selectedReference}
		<div class="p-5 sm:p-6">
			{#if contribution.discogs_source?.expired}
				<div class="alert alert-warning mb-4" role="status">
					<span
						>Discogs data is more than six hours old. Refresh it before comparing or submitting.</span
					>
				</div>
			{/if}
			<div class="flex flex-wrap items-start justify-between gap-4 rounded-box bg-base-100/55 p-4">
				<div class="min-w-0">
					<span class="badge badge-outline badge-sm"
						>Discogs release #{selectedReference.external_id}</span
					>
					<h3 class="mt-2 font-bold">{selected?.title ?? 'Selected release'}</h3>
					<p class="text-sm text-base-content/60">
						{selected?.artist_name ?? 'Refresh to display current Discogs metadata'}
						{#if selected?.country}
							· {selected.country}{/if}
						{#if selected?.released_date}
							· {selected.released_date}{/if}
					</p>
					<a
						class="link link-primary mt-2 inline-flex items-center gap-1 text-xs"
						href={selectedReference.canonical_url}
						target="_blank"
						rel="noopener noreferrer"
					>
						Data provided by Discogs <ExternalLink class="h-3 w-3" />
					</a>
				</div>
				<div class="flex gap-2">
					{#if canMutate}
						<Button
							variant="ghost"
							size="sm"
							icon={RefreshCw}
							loading={selectMutation.isPending}
							onclick={() => select(selectedReference.external_id)}>Refresh</Button
						>
						<Button
							variant="danger-ghost"
							size="sm"
							icon={Unlink}
							disabled={removeMutation.isPending}
							onclick={() =>
								removeMutation.mutate({
									contributionId: contribution.id,
									expectedRowRevision: contribution.row_revision
								})}>Remove</Button
						>
					{/if}
				</div>
			</div>
		</div>
	{:else}
		<form
			class="p-5 sm:p-6"
			onsubmit={(event) => {
				event.preventDefault();
				search();
			}}
		>
			<label class="block">
				<span class="label font-semibold">Discogs release</span>
				<div class="join w-full">
					<Input
						label="Discogs release"
						class="join-item min-w-0 flex-1"
						bind:value={query}
						placeholder="Artist, title, barcode, release URL, or ID"
						disabled={!canMutate}
					/>
					<Button
						type="submit"
						variant="primary"
						class="join-item"
						icon={Search}
						loading={searchMutation.isPending}
						disabled={!canMutate || selectMutation.isPending}
						>{exactInput ? 'Use release' : 'Search'}</Button
					>
				</div>
			</label>

			{#if searchMutation.data}
				<div class="mt-5" aria-live="polite">
					{#if searchMutation.data.results.length === 0}
						<p class="rounded-box bg-base-100/55 p-4 text-sm text-base-content/60">
							No matching Discogs releases. Try an exact release URL or a broader search.
						</p>
					{:else}
						<ul class="space-y-2">
							{#each searchMutation.data.results as result (result.release_id)}
								<li class="rounded-box border border-base-content/10 bg-base-100/35 p-4">
									<div class="flex flex-wrap items-start justify-between gap-3">
										<div class="min-w-0">
											<h3 class="font-bold">{result.title}</h3>
											<p class="text-sm text-base-content/60">
												{result.artist_name || 'Artist not listed'}
												{#if result.year}
													· {result.year}{/if}
												{#if result.country}
													· {result.country}{/if}
											</p>
											<p class="mt-1 text-xs text-base-content/45">
												{result.label ?? 'Label not listed'}
												{#if result.catalogue_number}
													· {result.catalogue_number}{/if}
												{#if result.format_summary}
													· {result.format_summary}{/if}
											</p>
											<a
												class="link link-primary mt-2 inline-flex items-center gap-1 text-xs"
												href={result.canonical_url}
												target="_blank"
												rel="noopener noreferrer"
											>
												Data provided by Discogs <ExternalLink class="h-3 w-3" />
											</a>
										</div>
										<Button
											variant="outline"
											size="sm"
											disabled={!canMutate || selectMutation.isPending}
											onclick={() => select(result.release_id)}>Select release</Button
										>
									</div>
								</li>
							{/each}
						</ul>
					{/if}
				</div>
			{/if}
		</form>
	{/if}
</Card>
