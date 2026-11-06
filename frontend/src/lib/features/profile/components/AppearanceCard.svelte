<script lang="ts">
	import { themeStore, type ThemePreference } from '$lib/stores/theme.svelte';
	import { customCssStore, MAX_CUSTOM_CSS } from '$lib/stores/customCss.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Field from '$lib/ui/Field.svelte';
	import RadioCards from '$lib/ui/RadioCards.svelte';

	const options: { value: ThemePreference; label: string; description: string }[] = [
		{ value: 'dark', label: 'Dark', description: 'The default look.' },
		{ value: 'light', label: 'Light', description: 'Light surfaces with darker accents.' },
		{
			value: 'system',
			label: 'System',
			description: "Follow this device's light or dark setting."
		}
	];

	let draft = $state(customCssStore.css);

	function save(): void {
		customCssStore.set(draft);
	}

	function reset(): void {
		draft = '';
		customCssStore.clear();
	}
</script>

<p class="pf-group" aria-hidden="true">Theme</p>
<RadioCards
	label="Theme"
	{options}
	value={themeStore.preference}
	onchange={(value) => themeStore.set(value)}
	class="sm:grid-cols-3"
/>
<p class="mt-2.5 text-xs text-muted">Saved in this browser only.</p>

<div class="mt-6 border-t border-line pt-5">
	<Field
		label="Custom CSS (advanced)"
		help="Applies to this browser only. CSS can hide controls or load remote images. Add ?nocustomcss=1 to the URL to turn it off."
	>
		{#snippet children(control)}
			<textarea
				{...control}
				class="textarea w-full font-mono text-sm"
				rows={8}
				maxlength={MAX_CUSTOM_CSS}
				spellcheck={false}
				bind:value={draft}></textarea>
		{/snippet}
	</Field>
	{#if customCssStore.disabled}
		<p class="mt-2 text-xs text-warning">
			Custom CSS is off for this page because ?nocustomcss=1 is in the URL.
		</p>
	{/if}
	<div class="mt-2 flex flex-wrap items-center justify-between gap-3">
		<span class="text-xs text-muted">{draft.length} / {MAX_CUSTOM_CSS}</span>
		<div class="flex gap-2">
			<Button variant="ghost" size="sm" disabled={!draft && !customCssStore.css} onclick={reset}>
				Reset
			</Button>
			<Button variant="primary" size="sm" disabled={draft === customCssStore.css} onclick={save}>
				Save
			</Button>
		</div>
	</div>
</div>
