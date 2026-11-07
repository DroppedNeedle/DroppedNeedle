<!--
	The theme's `--dn-*` variables with their values as the page shows them
	now, a swatch for each colour. A click hands `var(--name)` to `oninsert`.
	`revision` changes whenever the page's styles may have, so the values are
	read again.
-->
<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { themeStore } from '$lib/stores/theme.svelte';
	import { THEME_VARIABLES } from './cssVocabulary';

	interface Props {
		oninsert: (text: string) => void;
		disabled?: boolean;
		revision?: number;
	}

	let { oninsert, disabled = false, revision = 0 }: Props = $props();

	let filter = $state('');

	const shown = $derived(
		THEME_VARIABLES.filter((name) => name.includes(filter.trim().toLowerCase()))
	);

	const values = $derived.by(() => {
		void themeStore.resolved;
		void revision;
		const style = getComputedStyle(document.documentElement);
		return new Map(THEME_VARIABLES.map((name) => [name, style.getPropertyValue(name).trim()]));
	});

	function isColour(value: string): boolean {
		return value !== '' && CSS.supports('color', value);
	}
</script>

<TextField label="Filter variables" type="search" mono bind:value={filter} placeholder="--dn-" />
<ul class="css-rail-list mt-2" aria-label="Theme variables">
	{#each shown as name (name)}
		{@const value = values.get(name) ?? ''}
		<li>
			<Button
				variant="ghost"
				size="sm"
				class="w-full justify-start"
				{disabled}
				title="Insert var({name})"
				onclick={() => oninsert(`var(${name})`)}
			>
				{#if isColour(value)}
					<span class="css-swatch" style:background-color={value} aria-hidden="true"></span>
				{:else}
					<span class="css-swatch css-swatch--none" aria-hidden="true"></span>
				{/if}
				<span class="min-w-0 flex-1 truncate text-start font-mono text-xs">{name}</span>
				<span class="shrink-0 font-mono text-xs font-normal text-muted">{value}</span>
			</Button>
		</li>
	{:else}
		<li class="px-2 py-3 text-sm text-muted">No variable matches.</li>
	{/each}
</ul>
