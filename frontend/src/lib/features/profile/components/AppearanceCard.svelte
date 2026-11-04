<script lang="ts">
	import { SunMoon } from 'lucide-svelte';
	import { themeStore, type ThemePreference } from '$lib/stores/theme.svelte';
	import Card from '$lib/ui/Card.svelte';
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

<section>
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-muted"
	>
		<SunMoon class="h-4 w-4" />
		Appearance
	</h2>

	<Card variant="bordered" padding="md">
		<RadioCards
			label="Theme"
			{options}
			value={themeStore.preference}
			onchange={(value) => themeStore.set(value)}
			class="sm:grid-cols-3"
		/>
		<p class="mt-3 px-1 text-xs text-muted">Saved in this browser only.</p>
	</Card>
</section>
