<!--
	The settings sections. On a desktop: a filter and the three groups as
	folding paper heads (acid while open), each section an icon and a label,
	the current one an acid pill, with a connection light where a section has
	one. Below that width: one dropdown that lists every section.
-->
<script lang="ts" module>
	import type { Component } from 'svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	export interface RailTier {
		id: string;
		label: string;
		hint: string;
	}
	export interface RailTab {
		id: string;
		label: string;
		tier: string;
		/** A Lucide icon or a service's own mark. */
		icon: KitIcon | Component<{ class?: string }>;
	}
</script>

<script lang="ts">
	import { ChevronDown, CircleArrowUp, Search, Settings } from 'lucide-svelte';
	import Input from '$lib/ui/Input.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';

	interface Props {
		tiers: RailTier[];
		tabs: RailTab[];
		active: string;
		onselect: (id: string) => void;
		/** The section's connection light: null when it has none. */
		connected: (id: string) => boolean | null;
		updateAvailable: boolean;
	}

	let { tiers, tabs, active, onselect, connected, updateAvailable }: Props = $props();

	let filter = $state('');
	// A group the person folded or opened; otherwise only the current one is open.
	let folded = $state<Record<string, boolean>>({});
	// Bumped when a pick is refused (unsaved changes kept), so the dropdown shows the current section again.
	let pickKey = $state(0);

	const normalizedFilter = $derived(filter.trim().toLowerCase());
	const activeTier = $derived(tabs.find((t) => t.id === active)?.tier);

	function tabsForTier(tier: string) {
		return tabs.filter(
			(t) =>
				t.tier === tier && (!normalizedFilter || t.label.toLowerCase().includes(normalizedFilter))
		);
	}
	const noMatches = $derived(
		normalizedFilter.length > 0 && tiers.every((t) => tabsForTier(t.id).length === 0)
	);

	function isOpen(tier: string): boolean {
		if (normalizedFilter) return true;
		return folded[tier] ?? tier === activeTier;
	}

	function pick(id: string) {
		onselect(id);
		if (id !== active) pickKey += 1;
	}

	const pickerOptions = $derived(
		tiers.flatMap((tier) =>
			tabs
				.filter((t) => t.tier === tier.id)
				.map((t) => ({ value: t.id, label: t.label, group: tier.label }))
		)
	);
</script>

<div class="dn-rail-pick lg:hidden">
	<Settings class="dn-rail-pick__icon size-4" aria-hidden="true" />
	{#key pickKey}
		<SelectField
			label="Settings section"
			bare
			class="dn-rail-pick__select"
			bind:value={() => active, (next) => pick(next)}
			options={pickerOptions}
		/>
	{/key}
</div>

<nav class="dn-rail hidden lg:flex" aria-label="Settings sections">
	<Input
		size="sm"
		icon={Search}
		label="Filter settings"
		placeholder="Filter settings…"
		class="w-full"
		bind:value={filter}
	/>

	{#each tiers as tier (tier.id)}
		{@const tierTabs = tabsForTier(tier.id)}
		{@const open = isOpen(tier.id)}
		{#if tierTabs.length > 0}
			<section class="dn-rail__group">
				<button
					type="button"
					class="dn-rail__head"
					class:is-open={open}
					aria-expanded={open}
					aria-controls="settings-rail-{tier.id}"
					onclick={() => (folded = { ...folded, [tier.id]: !open })}
				>
					<span class="dn-rail__titles">
						<span class="dn-rail__title">{tier.label}</span>
						<span class="dn-rail__hint">{tier.hint}</span>
					</span>
					<ChevronDown class="dn-rail__chevron size-5" aria-hidden="true" />
				</button>
				{#if open}
					<ul class="dn-rail__list" id="settings-rail-{tier.id}">
						{#each tierTabs as tab (tab.id)}
							{@const Icon = tab.icon}
							{@const isActive = active === tab.id}
							{@const light = connected(tab.id)}
							<li>
								<button
									type="button"
									class="dn-rail__item"
									class:is-active={isActive}
									aria-current={isActive ? 'page' : undefined}
									onclick={() => pick(tab.id)}
								>
									<Icon class="dn-rail__icon size-[1.125rem]" />
									<span class="truncate">{tab.label}</span>
									{#if light !== null}
										<span class="dn-rail__light" class:is-on={light}>
											<span class="sr-only">{light ? 'Connected' : 'Not connected'}</span>
										</span>
									{/if}
									{#if tab.id === 'about' && updateAvailable}
										<span class="dn-rail__update">
											<CircleArrowUp class="size-3" aria-hidden="true" />
											Update
										</span>
									{/if}
								</button>
							</li>
						{/each}
					</ul>
				{/if}
			</section>
		{/if}
	{/each}

	{#if noMatches}
		<p class="px-3 py-2 text-sm text-muted">No settings match "{filter}".</p>
	{/if}
</nav>

<style>
	.dn-rail-pick {
		position: relative;
	}
	.dn-rail-pick :global(.dn-rail-pick__icon) {
		position: absolute;
		top: 50%;
		left: 0.875rem;
		z-index: 1;
		translate: 0 -50%;
		color: var(--dn-title);
		pointer-events: none;
	}
	.dn-rail-pick :global(.dn-rail-pick__select) {
		width: 100%;
		padding-left: 2.5rem;
		font-weight: 600;
	}

	.dn-rail {
		flex-direction: column;
		gap: 0.75rem;
	}

	.dn-rail__group {
		overflow: hidden;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
	}

	/* A folded group's head is paper; an open one is acid. */
	.dn-rail__head {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		width: 100%;
		padding: 0.625rem 0.875rem 0.5rem;
		border-bottom: 1px solid transparent;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		text-align: left;
		transition: background-color 150ms;
	}
	.dn-rail__head:hover {
		background-color: color-mix(in oklab, var(--dn-paper-content) 8%, var(--dn-paper));
	}
	.dn-rail__head.is-open {
		border-bottom-color: var(--dn-action-edge);
		background-color: var(--dn-action);
		color: var(--dn-action-content);
	}
	.dn-rail__head:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: -4px;
	}
	.dn-rail__titles {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
	}
	.dn-rail__title {
		font-family: var(--font-display);
		font-size: 1.375rem;
		font-weight: 400;
		line-height: 1.05;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.dn-rail__hint {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.06em;
	}
	.dn-rail__head :global(.dn-rail__chevron) {
		flex: none;
		transition: rotate 150ms;
	}
	.dn-rail__head.is-open :global(.dn-rail__chevron) {
		rotate: 180deg;
	}

	.dn-rail__list {
		display: flex;
		flex-direction: column;
		gap: 0.125rem;
		padding: 0.5rem;
	}

	.dn-rail__item {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		width: 100%;
		min-height: 2.25rem;
		padding: 0.375rem 0.75rem;
		border: 1px solid transparent;
		border-radius: 0.5rem;
		font-size: 0.9375rem;
		color: var(--color-base-content);
		text-align: left;
		transition: background-color 120ms;
	}
	.dn-rail__item:hover {
		background-color: var(--dn-selected);
	}
	.dn-rail__item:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 1px;
	}
	.dn-rail__item :global(.dn-rail__icon) {
		flex: none;
		color: var(--dn-muted);
	}
	.dn-rail__item.is-active {
		border-color: var(--dn-action-edge);
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		font-weight: 700;
	}
	.dn-rail__item.is-active :global(.dn-rail__icon) {
		color: var(--dn-action-content);
	}

	.dn-rail__light {
		flex: none;
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 9999px;
		background-color: color-mix(in oklab, var(--color-base-content) 28%, transparent);
	}
	.dn-rail__light.is-on {
		background-color: var(--color-success);
		box-shadow: 0 0 0 3px color-mix(in oklab, var(--color-success) 30%, transparent);
	}
	.dn-rail__item.is-active .dn-rail__light:not(.is-on) {
		background-color: color-mix(in oklab, var(--dn-action-content) 40%, transparent);
	}

	.dn-rail__update {
		display: inline-flex;
		flex: none;
		align-items: center;
		gap: 0.25rem;
		margin-left: auto;
		padding: 0.125rem 0.5rem;
		border: 1px solid currentColor;
		border-radius: 9999px;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	@media (forced-colors: active) {
		.dn-rail__head.is-open,
		.dn-rail__item.is-active {
			outline: 2px solid Highlight;
		}
	}
</style>
