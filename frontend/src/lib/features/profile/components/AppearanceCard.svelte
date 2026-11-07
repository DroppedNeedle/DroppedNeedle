<script lang="ts">
	import { untrack } from 'svelte';
	import { themeStore, type ThemePreference } from '$lib/stores/theme.svelte';
	import {
		customCssProblem,
		customCssSaveError,
		customCssStore,
		MAX_CUSTOM_CSS
	} from '$lib/stores/customCss.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { createSaveAppearanceMutation } from '$lib/features/profile/queries/appearance.svelte';
	import Button from '$lib/ui/Button.svelte';
	import RadioCards from '$lib/ui/RadioCards.svelte';
	import TextAreaField from '$lib/ui/TextAreaField.svelte';

	const options: { value: ThemePreference; label: string; description: string }[] = [
		{ value: 'dark', label: 'Dark', description: 'The default look.' },
		{ value: 'light', label: 'Light', description: 'Light surfaces with darker accents.' },
		{ value: 'midnight', label: 'Midnight', description: 'Dark, with dark paper cards.' },
		{
			value: 'system',
			label: 'System',
			description: "Follow this device's light or dark setting."
		}
	];

	const saveTheme = createSaveAppearanceMutation();
	const saveCssMutation = createSaveAppearanceMutation();

	let draft = $state('');
	let cssError = $state<string | null>(null);
	let seededFor: string | null = null;

	// Seed the box once per loaded user, so a save or an account switch
	// never leaves another person's text in it.
	$effect(() => {
		const owner = customCssStore.owner;
		if (owner === seededFor) return;
		seededFor = owner;
		untrack(() => {
			draft = customCssStore.css;
			cssError = draft ? customCssProblem(draft) : null;
		});
	});

	// Shown at once; a failed save keeps it on screen and the next load corrects it.
	function chooseTheme(value: ThemePreference): void {
		themeStore.set(value);
		saveTheme.mutate(
			{ theme: value },
			{
				onError: () =>
					toastStore.show({ message: 'Theme not saved to your account.', type: 'error' })
			}
		);
	}

	function saveCss(next: string): void {
		cssError = customCssStore.saveProblem(next);
		if (cssError) return;
		saveCssMutation.mutate(
			{ custom_css: { text: next } },
			{
				onSuccess: () => {
					draft = next;
				},
				onError: (error) => {
					cssError = customCssSaveError(error);
				}
			}
		);
	}
</script>

<p class="pf-group" aria-hidden="true">Theme</p>
<RadioCards
	label="Theme"
	{options}
	value={themeStore.preference}
	onchange={chooseTheme}
	class="sm:grid-cols-2 lg:grid-cols-4"
/>
<p class="mt-2.5 text-xs text-muted">Saved to your account.</p>

<details class="mt-6 border-t border-line pt-5">
	<summary class="cursor-pointer text-sm font-semibold">Custom CSS</summary>
	<div class="mt-3">
		<TextAreaField
			label="CSS rules"
			help="Saved to your account. Add ?nocustomcss=1 to the address to turn it off."
			error={cssError}
			mono
			rows={6}
			maxlength={MAX_CUSTOM_CSS}
			capped
			bind:value={draft}
		/>
		{#if customCssStore.disabled}
			<p class="mt-2 text-xs text-warning">Off until the page is reloaded.</p>
		{/if}
		<div class="mt-2 flex justify-end gap-2">
			<Button
				variant="ghost"
				size="sm"
				disabled={!draft && !customCssStore.css}
				onclick={() => saveCss('')}
			>
				Reset
			</Button>
			<Button
				variant="primary"
				size="sm"
				disabled={draft === customCssStore.css || saveCssMutation.isPending}
				onclick={() => saveCss(draft)}
			>
				Save
			</Button>
		</div>
	</div>
</details>
