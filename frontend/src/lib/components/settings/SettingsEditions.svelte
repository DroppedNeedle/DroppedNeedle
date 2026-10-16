<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { onDestroy, onMount } from 'svelte';
	import { ArrowDown, ArrowUp, Disc3, Plus, X } from 'lucide-svelte';
	import type { EditionPreferences } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

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

	const dateOptions: { value: EditionPreferences['date']; label: string }[] = [
		{ value: 'earliest', label: 'Earliest (usually the original)' },
		{ value: 'latest', label: 'Latest (usually the newest remaster)' },
		{ value: 'any', label: "Doesn't matter" }
	];
	const versionOptions: { value: EditionPreferences['version']; label: string }[] = [
		{ value: 'standard', label: 'The standard album' },
		{ value: 'deluxe', label: 'Deluxe and expanded editions' },
		{ value: 'any', label: "Doesn't matter" }
	];

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

<Card>
	{#snippet header()}
		<div class="flex items-center gap-2">
			<Disc3 class="h-5 w-5 text-primary" aria-hidden="true" />
			<h2 class="card-title">Edition preferences</h2>
		</div>
	{/snippet}
	<div class="flex flex-col gap-4">
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
					<Card as="section" variant="bordered" padding="md">
						<h3 class="font-semibold">{list.title}</h3>
						<p class="grow-0 text-xs text-base-content/55">{list.hint}</p>
						<ol class="mt-3 space-y-1">
							{#each form.data[list.key] as entry, index (entry)}
								<li class="flex items-center gap-2 rounded-lg bg-base-200/60 px-2 py-1 text-sm">
									<span class="w-5 text-xs text-base-content/45">{index + 1}.</span>
									<span class="min-w-0 flex-1 truncate">{entry}</span>
									<IconButton
										size="xs"
										shape="square"
										icon={ArrowUp}
										label={`Move ${entry} up`}
										disabled={index === 0}
										onclick={() => move(list.key, index, -1)}
									/>
									<IconButton
										size="xs"
										shape="square"
										icon={ArrowDown}
										label={`Move ${entry} down`}
										disabled={index === form.data[list.key].length - 1}
										onclick={() => move(list.key, index, 1)}
									/>
									<IconButton
										size="xs"
										shape="square"
										icon={X}
										label={`Remove ${entry}`}
										onclick={() => remove(list.key, index)}
									/>
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
							<Input
								size="sm"
								class="join-item w-full"
								placeholder={list.placeholder}
								label={`Add to ${list.title}`}
								bind:value={drafts[list.key]}
							/>
							<IconButton
								type="submit"
								variant="neutral"
								size="sm"
								shape="square"
								class="join-item"
								icon={Plus}
								label="Add"
							/>
						</form>
					</Card>
				{/each}
			</div>

			<div class="grid gap-4 md:grid-cols-2">
				<SelectField label="Release date" options={dateOptions} bind:value={form.data.date} />
				<SelectField
					label="Standard or deluxe"
					options={versionOptions}
					bind:value={form.data.version}
				/>
			</div>

			<fieldset>
				<legend class="text-sm font-medium">Avoid unless nothing else fits</legend>
				<div class="mt-2 flex flex-wrap gap-3">
					{#each avoidable as kind (kind)}
						<label class="label cursor-pointer gap-2">
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={form.data.avoid_types.includes(kind)}
								onchange={(event) => toggleAvoid(kind, event.currentTarget.checked)}
							/>
							<span class="capitalize">{kind}</span>
						</label>
					{/each}
				</div>
			</fieldset>

			<Card variant="bordered" padding="md">
				<ToggleRow
					label="Also manage unconfirmed matches"
					description="When DroppedNeedle isn't sure of an album's edition, it still uses its best guess for pages, playback and downloads, but leaves the files alone until you confirm it. Turn this on to let tagging and organizing work on those albums too."
					bind:checked={form.data.manage_unconfirmed}
				/>
			</Card>

			<div class="flex justify-end">
				<Button variant="primary" size="sm" loading={form.saving} onclick={() => form.save()}>
					Save
				</Button>
			</div>
			{#if form.message}
				<p class="text-sm {form.messageType === 'error' ? 'text-error' : 'text-success'}">
					{form.message}
				</p>
			{/if}
		{/if}
	</div>
</Card>
