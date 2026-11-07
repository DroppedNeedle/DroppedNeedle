<!-- The theme cards. A choice shows at once and is saved to the account. -->
<script lang="ts">
	import { themeStore, type ThemePreference } from '$lib/stores/theme.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { claimThemeCopy } from '$lib/features/profile/state/appearance.svelte';
	import { createSaveAppearanceMutation } from '$lib/features/profile/queries/appearance.svelte';
	import RadioCards from '$lib/ui/RadioCards.svelte';

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

	// A failed save keeps the choice on screen; the next load corrects it.
	function chooseTheme(value: ThemePreference): void {
		themeStore.set(value);
		claimThemeCopy();
		saveTheme.mutate(
			{ theme: value },
			{
				onError: () =>
					toastStore.show({ message: 'Theme not saved to your account.', type: 'error' })
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
	class="grid-cols-2 lg:grid-cols-4"
/>
<p class="mt-2.5 text-xs text-muted">Saved to your account.</p>
