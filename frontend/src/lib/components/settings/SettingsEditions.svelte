<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { onDestroy, onMount } from 'svelte';
	import { ArrowDown, ArrowUp, Disc3, Plus, X } from 'lucide-svelte';
	import type { EditionPreferences } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';

	const form = createSettingsForm<EditionPreferences>({
		loadEndpoint: SETTINGS_ENDPOINTS.editionPreferences(),
		saveEndpoint: SETTINGS_ENDPOINTS.editionPreferences()
	});

	onMount(() => form.load());
	onDestroy(() => form.cleanup());

	type ListKey = 'status_order' | 'format_order' | 'countries';

	const lists: { key: ListKey; title: string; hint: string; placeholder: string }[] = [
		{
			key: 'status_order',
			title: 'Release status',
			hint: 'Official releases first, then promos, then bootlegs.',
			placeholder: 'e.g. official'
		},
		{
			key: 'format_order',
			title: 'Format',
			hint: 'A format matches when its name contains the word, so "vinyl" covers 7", 10" and 12".',
			placeholder: 'e.g. digital media'
		},
		{
			key: 'countries',
			title: 'Countries',
			hint: 'Two-letter codes; XW is worldwide. Leave empty to use your store region, then worldwide.',
			placeholder: 'e.g. GB'
		}
	];

	const avoidable = ['live', 'compilation', 'remix', 'soundtrack', 'demo', 'dj-mix'];

	let drafts = $state<Record<ListKey, string>>({
		status_order: '',
		format_order: '',
		countries: ''
	});

	function move(key: ListKey, index: number, by: number): void {
		if (!form.data) return;
		const list = [...form.data[key]];
		const target = index + by;
		if (target < 0 || target >= list.length) return;
		[list[index], list[target]] = [list[target], list[index]];
		form.data[key] = list;
	}

	function remove(key: ListKey, index: number): void {
		if (!form.data) return;
		form.data[key] = form.data[key].filter((_, i) => i !== index);
	}

	function add(key: ListKey): void {
		if (!form.data) return;
		const value = drafts[key].trim();
		if (!value) return;
		const normalized = key === 'countries' ? value.toUpperCase() : value.toLowerCase();
		if (!form.data[key].includes(normalized)) form.data[key] = [...form.data[key], normalized];
		drafts[key] = '';
	}

	function toggleAvoid(kind: string, on: boolean): void {
		if (!form.data) return;
		const rest = form.data.avoid_types.filter((entry) => entry !== kind);
		form.data.avoid_types = on ? [...rest, kind] : rest;
	}
</script>

<div class="card bg-base-200">
	<div class="card-body gap-4">
		<div class="flex items-center gap-2">
			<Disc3 class="h-5 w-5 text-primary" aria-hidden="true" />
			<h2 class="card-title">Edition preferences</h2>
		</div>
		<p class="text-sm text-base-content/60">
			Most albums come in several editions: the original CD, a remaster, a deluxe set, a vinyl
			pressing. DroppedNeedle picks the edition that fits your files best. When two editions fit
			equally well, or when you request an album you don't have yet, it uses these preferences. An
			edition you pick yourself always wins.
		</p>

		{#if form.loading}
			<div class="space-y-3">
				<div class="skeleton h-24 w-full rounded-xl"></div>
				<div class="skeleton h-24 w-full rounded-xl"></div>
			</div>
		{:else if form.data}
			<div class="grid gap-4 lg:grid-cols-3">
				{#each lists as list (list.key)}
					<section class="rounded-box border border-base-content/10 bg-base-100 p-4">
						<h3 class="font-semibold">{list.title}</h3>
						<p class="mt-1 text-xs text-base-content/55">{list.hint}</p>
						<ol class="mt-3 space-y-1">
							{#each form.data[list.key] as entry, index (entry)}
								<li class="flex items-center gap-2 rounded-lg bg-base-200/60 px-2 py-1 text-sm">
									<span class="w-5 text-xs text-base-content/45">{index + 1}.</span>
									<span class="min-w-0 flex-1 truncate">{entry}</span>
									<button
										type="button"
										class="btn btn-ghost btn-xs btn-square"
										aria-label={`Move ${entry} up`}
										disabled={index === 0}
										onclick={() => move(list.key, index, -1)}><ArrowUp class="h-3 w-3" /></button
									>
									<button
										type="button"
										class="btn btn-ghost btn-xs btn-square"
										aria-label={`Move ${entry} down`}
										disabled={index === form.data[list.key].length - 1}
										onclick={() => move(list.key, index, 1)}><ArrowDown class="h-3 w-3" /></button
									>
									<button
										type="button"
										class="btn btn-ghost btn-xs btn-square"
										aria-label={`Remove ${entry}`}
										onclick={() => remove(list.key, index)}><X class="h-3 w-3" /></button
									>
								</li>
							{:else}
								<li class="text-xs text-base-content/45">
									{list.key === 'countries' ? 'Store region, then worldwide.' : 'No preference.'}
								</li>
							{/each}
						</ol>
						<form
							class="join mt-3 w-full"
							onsubmit={(event) => {
								event.preventDefault();
								add(list.key);
							}}
						>
							<input
								class="input input-bordered input-sm join-item w-full"
								placeholder={list.placeholder}
								aria-label={`Add to ${list.title}`}
								bind:value={drafts[list.key]}
							/>
							<button class="btn btn-sm join-item" type="submit" aria-label="Add"
								><Plus class="h-4 w-4" /></button
							>
						</form>
					</section>
				{/each}
			</div>

			<div class="grid gap-4 md:grid-cols-2">
				<label class="form-control">
					<span class="label-text font-medium">Release date</span>
					<select class="select select-bordered select-sm mt-1" bind:value={form.data.date}>
						<option value="earliest">Earliest (usually the original)</option>
						<option value="latest">Latest (usually the newest remaster)</option>
						<option value="any">Doesn't matter</option>
					</select>
				</label>
				<label class="form-control">
					<span class="label-text font-medium">Standard or deluxe</span>
					<select class="select select-bordered select-sm mt-1" bind:value={form.data.version}>
						<option value="standard">The standard album</option>
						<option value="deluxe">Deluxe and expanded editions</option>
						<option value="any">Doesn't matter</option>
					</select>
				</label>
			</div>

			<fieldset>
				<legend class="label-text font-medium">Avoid unless nothing else fits</legend>
				<div class="mt-2 flex flex-wrap gap-3">
					{#each avoidable as kind (kind)}
						<label class="label cursor-pointer gap-2">
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={form.data.avoid_types.includes(kind)}
								onchange={(event) => toggleAvoid(kind, event.currentTarget.checked)}
							/>
							<span class="label-text capitalize">{kind}</span>
						</label>
					{/each}
				</div>
			</fieldset>

			<div class="rounded-box border border-base-content/10 bg-base-100 p-4">
				<label class="label cursor-pointer justify-start gap-3">
					<input
						type="checkbox"
						class="toggle toggle-primary toggle-sm"
						bind:checked={form.data.manage_unconfirmed}
					/>
					<span class="label-text font-medium">Also manage unconfirmed matches</span>
				</label>
				<p class="text-xs text-base-content/55">
					When DroppedNeedle isn't sure of an album's edition, it still uses its best guess for
					pages, playback and downloads, but leaves the files alone until you confirm it. Turn this
					on to let tagging and organizing work on those albums too.
				</p>
			</div>

			<div class="card-actions justify-end">
				<button class="btn btn-primary btn-sm" onclick={() => form.save()} disabled={form.saving}>
					{form.saving ? 'Saving…' : 'Save'}
				</button>
			</div>
			{#if form.message}
				<p class="text-sm {form.messageType === 'error' ? 'text-error' : 'text-success'}">
					{form.message}
				</p>
			{/if}
		{/if}
	</div>
</div>
