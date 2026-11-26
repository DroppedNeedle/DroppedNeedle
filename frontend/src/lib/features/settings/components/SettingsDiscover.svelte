<script lang="ts">
	import SectionPrefsManager from './SectionPrefsManager.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import {
		createSaveDiscoverPrefsMutation,
		getDiscoverPrefsQuery,
		type SetDay
	} from '$lib/features/discover/queries/week.svelte';

	const prefsQuery = getDiscoverPrefsQuery();
	const save = createSaveDiscoverPrefsMutation();

	const DAYS: { value: SetDay; label: string }[] = [
		{ value: 'monday', label: 'Monday' },
		{ value: 'tuesday', label: 'Tuesday' },
		{ value: 'wednesday', label: 'Wednesday' },
		{ value: 'thursday', label: 'Thursday' },
		{ value: 'friday', label: 'Friday' },
		{ value: 'saturday', label: 'Saturday' },
		{ value: 'sunday', label: 'Sunday' }
	];

	// the day just chosen shows while it saves
	let chosen = $state<SetDay | null>(null);
	const setDay = $derived(chosen ?? prefsQuery.data?.set_day ?? 'friday');

	function choose(day: SetDay) {
		const prefs = prefsQuery.data;
		if (!prefs) return;
		chosen = day;
		save.mutate({ ...prefs, set_day: day }, { onSettled: () => (chosen = null) });
	}
</script>

<div class="space-y-5">
	<SectionPrefsManager
		page="discover"
		title="Discover"
		description="Pick which discovery sections appear on your Discover page. Changes save automatically and only affect your account."
	/>
	<Card variant="bordered">
		<SelectField
			label="New set on"
			help="The day your weekly set comes out. This week's set stays as it is."
			options={DAYS}
			disabled={!prefsQuery.data}
			bind:value={() => setDay, choose}
		/>
	</Card>
</div>
