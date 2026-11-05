<script lang="ts">
	import { themeStore, type ThemePreference } from '$lib/stores/theme.svelte';
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
