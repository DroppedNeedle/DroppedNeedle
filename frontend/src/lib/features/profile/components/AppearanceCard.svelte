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

<details class="mt-6 border-t border-line pt-5">
	<summary class="cursor-pointer text-sm font-semibold">Custom CSS (advanced)</summary>
	<div class="mt-3">
		<Field
			label="CSS rules"
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
		<details class="mt-3 text-xs text-muted">
			<summary class="cursor-pointer font-semibold hover:text-base-content">How this works</summary>
			<div class="mt-2 space-y-2">
				<p>
					Paste CSS that overrides the app's design tokens or any class. The main tokens are
					<code>--color-*</code>, <code>--dn-*</code> and <code>--radius-*</code>.
				</p>
				<p>
					Saved in this browser only. Save applies it immediately; Reset clears it. Limit:
					{MAX_CUSTOM_CSS.toLocaleString()} characters.
				</p>
				<p>
					Only paste CSS you trust. It can hide controls or load remote images. If the page becomes
					hard to use, add <code>?nocustomcss=1</code> to the URL and reload.
				</p>
				<pre
					class="overflow-x-auto rounded bg-base-300 p-2 font-mono text-[11px] leading-relaxed text-base-content"><code
						>:root, [data-theme] &#123;
  --radius-box: 0 !important;
  --radius-field: 0 !important;
  --dn-muted: #8a8f8a !important;
&#125;</code
					></pre>
			</div>
		</details>
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
</details>
