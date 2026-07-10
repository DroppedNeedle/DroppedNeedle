<script lang="ts">
	import SectionPrefsManager from './SectionPrefsManager.svelte';
	import SettingsGenreBalance from './SettingsGenreBalance.svelte';
</script>

<div class="space-y-6">
	<SectionPrefsManager
		page="discover"
		title="Discover"
		description="Pick which discovery sections appear on your Discover page. Changes save automatically and only affect your account."
	/>

	<SettingsGenreBalance />
</div>
