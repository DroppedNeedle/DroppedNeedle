<script lang="ts">
	import { SunMoon } from 'lucide-svelte';
	import { themeStore, type ThemePreference } from '$lib/stores/theme.svelte';

	const options: { value: ThemePreference; label: string; hint: string }[] = [
		{ value: 'dark', label: 'Dark', hint: 'The default look.' },
		{ value: 'light', label: 'Light', hint: 'Light surfaces with darker accents.' },
		{ value: 'system', label: 'System', hint: "Follow this device's light or dark setting." }
	];
</script>

<section>
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<SunMoon class="h-4 w-4" />
		Appearance
	</h2>

	<div
		class="space-y-3 rounded-2xl border border-base-300/50 bg-base-200/40 p-4 backdrop-blur-sm sm:p-5"
	>
		<fieldset class="grid gap-3 sm:grid-cols-3">
			<legend class="sr-only">Theme</legend>
			{#each options as option (option.value)}
				<label
					class="flex cursor-pointer items-start gap-3 rounded-lg border border-base-300/50 bg-base-100/30 p-4"
				>
					<input
						class="radio radio-primary radio-sm mt-0.5"
						type="radio"
						name="theme"
						checked={themeStore.preference === option.value}
						onchange={() => themeStore.set(option.value)}
					/>
					<span>
						<span class="block text-sm font-medium">{option.label}</span>
						<span class="mt-1 block text-xs text-base-content/50">{option.hint}</span>
					</span>
				</label>
			{/each}
		</fieldset>
		<p class="px-1 text-xs text-base-content/40">Saved in this browser only.</p>
	</div>
</section>
