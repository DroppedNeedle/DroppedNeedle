<script lang="ts">
	import { resolve } from '$app/paths';
	import { onDestroy } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import { Check, ExternalLink } from 'lucide-svelte';
	import type { SectionPrefItem } from '$lib/types';
	import {
		getSectionPrefsQuery,
		saveSectionPrefs
	} from '$lib/queries/section-prefs/SectionPrefsQuery.svelte';
	import { toastStore } from '$lib/stores/toast';

	interface Props {
		page: 'home' | 'discover' | 'sidebar';
		title: string;
		description: string;
	}

	let { page, title, description }: Props = $props();

	const prefsQuery = getSectionPrefsQuery();

	// local working copy so toggles apply instantly; server state re-syncs it
	let sections = $state<SectionPrefItem[]>([]);
	let loadedFor = $state('');
	let saveTimer: ReturnType<typeof setTimeout> | null = null;
	let saving = $state(false);
	let savedFlash = $state(false);
	let savedFlashTimer: ReturnType<typeof setTimeout> | null = null;

	$effect(() => {
		const serverSections = prefsQuery.data?.pages?.[page];
		// adopt server state on first load / page switch, but never mid-edit
		if (serverSections && loadedFor !== page && !saveTimer && !saving) {
			sections = serverSections.map((s) => ({ ...s }));
			loadedFor = page;
		}
	});

	const zones = $derived.by(() => {
		// group across the WHOLE list (not consecutive runs): a zone appearing twice
		// would duplicate the {#each} key and crash the page
		const grouped: { zone: string; items: SectionPrefItem[] }[] = [];
		const byZone = new SvelteMap<string, { zone: string; items: SectionPrefItem[] }>();
		for (const s of sections) {
			let group = byZone.get(s.zone);
			if (!group) {
				group = { zone: s.zone, items: [] };
				byZone.set(s.zone, group);
				grouped.push(group);
			}
			group.items.push(s);
		}
		return grouped;
	});

	const enabledCount = $derived(sections.filter((s) => s.enabled).length);
	const allOn = $derived(sections.length > 0 && enabledCount === sections.length);
	const allOff = $derived(enabledCount === 0);

	function scheduleSave() {
		if (saveTimer) clearTimeout(saveTimer);
		saveTimer = setTimeout(() => void flushSave(), 400);
	}

	async function flushSave() {
		saveTimer = null;
		saving = true;
		try {
			await saveSectionPrefs({
				page,
				sections: sections.map((s) => ({ key: s.key, enabled: s.enabled }))
			});
			savedFlash = true;
			if (savedFlashTimer) clearTimeout(savedFlashTimer);
			savedFlashTimer = setTimeout(() => (savedFlash = false), 1500);
		} catch {
			toastStore.show({ message: "Couldn't save your section settings", type: 'error' });
			// revert to server truth
			loadedFor = '';
			await prefsQuery.refetch();
		} finally {
			saving = false;
		}
	}

	function toggleSection(key: string) {
		sections = sections.map((s) => (s.key === key ? { ...s, enabled: !s.enabled } : s));
		scheduleSave();
	}

	function toggleAll(enabled: boolean) {
		sections = sections.map((s) => ({ ...s, enabled }));
		scheduleSave();
	}

	onDestroy(() => {
		if (saveTimer) {
			clearTimeout(saveTimer);
			void flushSave();
		}
		if (savedFlashTimer) clearTimeout(savedFlashTimer);
	});
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<div class="flex items-start justify-between gap-3">
			<div>
				<h2 class="card-title text-2xl">{title}</h2>
				<p class="text-base-content/70">{description}</p>
			</div>
			<div
				class="flex items-center gap-1.5 text-xs text-success transition-opacity duration-300"
				class:opacity-0={!savedFlash}
				aria-hidden={!savedFlash}
			>
				<Check class="h-3.5 w-3.5" />
				Saved
			</div>
		</div>

		{#if prefsQuery.isLoading && sections.length === 0}
			<div class="flex justify-center items-center py-12">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if prefsQuery.isError && sections.length === 0}
			<div class="alert alert-error mt-4">
				<span>Couldn't load your section settings.</span>
				<button class="btn btn-sm" onclick={() => prefsQuery.refetch()}>Retry</button>
			</div>
		{:else if sections.length > 0}
			<label
				class="label mt-2 cursor-pointer justify-start gap-4 rounded-xl bg-base-100/60 px-4 py-3"
			>
				<input
					type="checkbox"
					class="toggle toggle-primary"
					checked={allOn}
					indeterminate={!allOn && !allOff}
					onchange={() => toggleAll(!allOn)}
				/>
				<div>
					<span class="label-text font-semibold">All sections</span>
					<p class="text-xs text-base-content/50">
						{enabledCount} of {sections.length} sections shown
					</p>
				</div>
			</label>

			<div class="mt-2 space-y-5">
				{#each zones as group (group.zone)}
					<div>
						<h3
							class="mb-1 px-1 text-xs font-semibold uppercase tracking-widest text-base-content/40"
						>
							{group.zone}
						</h3>
						<div class="divide-y divide-base-content/5 rounded-xl bg-base-100/40">
							{#each group.items as section (section.key)}
								<label
									class="label cursor-pointer justify-start gap-4 px-4 py-3 {section.available
										? ''
										: 'opacity-55'}"
								>
									<input
										type="checkbox"
										class="toggle toggle-primary toggle-sm"
										checked={section.enabled}
										disabled={!section.available}
										onchange={() => toggleSection(section.key)}
									/>
									<div class="min-w-0 flex-1">
										<span class="label-text font-medium">{section.title}</span>
										<p class="text-xs text-base-content/50">{section.description}</p>
									</div>
									{#if !section.available && section.requires}
										<a
											href={resolve('/settings?tab=connect-apps')}
											class="link flex shrink-0 items-center gap-1 text-xs text-primary/80"
										>
											Connect {section.requires === 'listenbrainz'
												? 'ListenBrainz'
												: section.requires === 'lastfm'
													? 'Last.fm'
													: section.requires}
											<ExternalLink class="h-3 w-3" />
										</a>
									{/if}
								</label>
							{/each}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
