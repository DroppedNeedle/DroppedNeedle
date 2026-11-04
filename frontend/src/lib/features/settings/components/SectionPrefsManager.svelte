<script lang="ts">
	import { onDestroy } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import { Check, ExternalLink } from 'lucide-svelte';
	import type {
		SectionPage,
		SectionPrefItem
	} from '$lib/features/settings/queries/section-prefs.svelte';
	import {
		getSectionPrefsQuery,
		saveSectionPrefs
	} from '$lib/features/settings/queries/section-prefs.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		page: SectionPage;
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

	// Scrobble accounts are each user's own, on /profile. Anything else is
	// an admin's settings tab, so a member is told to ask instead.
	function connectLink(requires: string): { href: string; label: string } | null {
		if (requires === 'lastfm') {
			return { href: withBasePath('/profile#scrobbling'), label: 'Connect Last.fm' };
		}
		if (requires === 'listenbrainz') {
			return { href: withBasePath('/profile#scrobbling'), label: 'Connect ListenBrainz' };
		}
		if (!authStore.isAdmin) return null;
		return { href: withBasePath(`/settings?tab=${requires}`), label: 'Open settings' };
	}

	onDestroy(() => {
		if (saveTimer) {
			clearTimeout(saveTimer);
			void flushSave();
		}
		if (savedFlashTimer) clearTimeout(savedFlashTimer);
	});
</script>

<Card>
	{#snippet header()}
		<div class="flex items-start justify-between gap-3">
			<div>
				<CardTitle class="text-2xl">{title}</CardTitle>
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
	{/snippet}

	{#if prefsQuery.isLoading && sections.length === 0}
		<div class="flex justify-center items-center py-12">
			<span class="loading loading-spinner loading-lg"></span>
		</div>
	{:else if prefsQuery.isError && sections.length === 0}
		<div class="alert alert-error mt-4">
			<span>Couldn't load your section settings.</span>
			<Button size="sm" onclick={() => prefsQuery.refetch()}>Retry</Button>
		</div>
	{:else if sections.length > 0}
		<label
			class="mt-2 flex cursor-pointer items-start justify-between gap-3 rounded-xl bg-base-100/60 px-4 py-3"
		>
			<span class="min-w-0">
				<span class="block text-sm font-semibold">All sections</span>
				<span class="mt-0.5 block text-xs text-base-content/50">
					{enabledCount} of {sections.length} sections shown
				</span>
			</span>
			<input
				type="checkbox"
				class="toggle toggle-sm toggle-primary shrink-0"
				checked={allOn}
				indeterminate={!allOn && !allOff}
				onchange={() => toggleAll(!allOn)}
			/>
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
							<div class="px-4 py-3 {section.available ? '' : 'opacity-55'}">
								<ToggleRow
									label={section.title}
									description={section.description}
									bind:checked={() => section.enabled, () => toggleSection(section.key)}
									disabled={!section.available}
								/>
								{#if !section.available && section.requires}
									{@const link = connectLink(section.requires)}
									{#if link}
										<a
											href={link.href}
											class="link mt-1 inline-flex items-center gap-1 text-xs text-primary/80"
										>
											{link.label}
											<ExternalLink class="h-3 w-3" />
										</a>
									{:else}
										<p class="mt-1 text-xs text-base-content/60">Ask an admin to set this up.</p>
									{/if}
								{/if}
							</div>
						{/each}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</Card>
