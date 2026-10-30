//! Where each source lives and how fast it may be polled.
//!
//! These were constants until the sources needed testing: a sync is mostly
//! fetch-shaped-into-record logic, and none of it could be exercised while the
//! host was baked in. Holding them as values costs one parameter per sync and
//! buys the whole normalisation path a test.

use crate::http::FetchOpts;

#[derive(Clone)]
pub struct Endpoints {
    /// They Vote For You API root, no trailing slash.
    pub tvfy: String,
    /// Parliamentary Handbook OData root, no trailing slash.
    pub handbook: String,
    /// ParlWork API root: the SPA behind APH's own bills list.
    pub parlwork: String,
    pub wikidata_sparql: String,
    pub commons_api: String,
    /// Commons file host, the one that serves Special:FilePath thumbnails.
    pub commons_files: String,
    /// AEC results host, the parent of the per-event download directories.
    pub aec_results: String,
    /// AEC website root serving the electorate profile pages.
    pub aec_profiles: String,
    /// Federal Register of Legislation API root.
    pub legislation: String,
    /// data.gov.au's CKAN action API.
    pub data_gov: String,
    /// The AEC Transparency Register, home of the bulk disclosure downloads.
    pub aec_transparency: String,
    /// The ABS's ArcGIS services root, home of the boundary sets.
    pub abs_geo: String,
    /// The Parliament of Victoria's website, home of its member search API.
    pub vic_parliament: String,
    /// The Queensland Parliament's documents host, which serves its member
    /// mailing lists.
    pub qld_documents: String,
    /// The Parliament of Tasmania's website.
    pub tas_parliament: String,
    /// The ACT Legislative Assembly's website.
    pub act_parliament: String,
    /// The Parliament of South Australia's public member contact API.
    pub sa_contacts: String,
    /// The Parliament of South Australia's members app API.
    pub sa_members: String,
    /// Victoria's legislation website, which keeps the Parliament's bills.
    pub vic_legislation: String,
    /// The ACT Legislation Register, which publishes each Assembly bill.
    pub act_legislation: String,
    /// The Northern Territory's legislation database, which keeps the
    /// Assembly's bills.
    pub nt_legislation: String,
    /// Queensland's legislation register, whose browse lists carry every
    /// bill as introduced and as passed.
    pub qld_legislation: String,
    /// The API behind the Parliament of South Australia's legislative
    /// tracker, SALT.
    pub sa_salt: String,
    /// Replaces every source's own per-host spacing when set. Only tests set
    /// it; the real intervals are what keeps these APIs willing to answer.
    pub min_interval_ms: Option<u64>,
    /// Retry backoff base, in the same spirit.
    pub backoff_ms: Option<u64>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            tvfy: "https://theyvoteforyou.org.au/api/v1".to_string(),
            handbook: "https://handbookapi.aph.gov.au/api".to_string(),
            parlwork: "https://parlwork.aph.gov.au/api".to_string(),
            wikidata_sparql: "https://query.wikidata.org/sparql".to_string(),
            commons_api: "https://commons.wikimedia.org/w/api.php".to_string(),
            commons_files: "https://commons.wikimedia.org".to_string(),
            aec_results: "https://results.aec.gov.au".to_string(),
            aec_profiles: "https://www.aec.gov.au".to_string(),
            legislation: "https://api.prod.legislation.gov.au/v1".to_string(),
            data_gov: "https://data.gov.au/data/api/3/action".to_string(),
            aec_transparency: "https://transparency.aec.gov.au".to_string(),
            abs_geo: "https://geo.abs.gov.au/arcgis/rest/services".to_string(),
            vic_parliament: "https://www.parliament.vic.gov.au".to_string(),
            qld_documents: "https://documents.parliament.qld.gov.au".to_string(),
            tas_parliament: "https://www.parliament.tas.gov.au".to_string(),
            act_parliament: "https://www.parliament.act.gov.au".to_string(),
            sa_contacts: "https://contact-details-api.parliament.sa.gov.au/api".to_string(),
            sa_members: "https://membersapp.parliament.sa.gov.au/api".to_string(),
            vic_legislation: "https://www.legislation.vic.gov.au".to_string(),
            act_legislation: "https://www.legislation.act.gov.au".to_string(),
            nt_legislation: "https://legislation.nt.gov.au".to_string(),
            qld_legislation: "https://www.legislation.qld.gov.au".to_string(),
            sa_salt: "https://prod-internet-salt-functionapp.azurewebsites.net/api".to_string(),
            min_interval_ms: None,
            backoff_ms: None,
        }
    }
}

impl Endpoints {
    /// Fetch options carrying this source's polite spacing, unless the config
    /// overrides it.
    pub fn opts(&self, min_interval_ms: u64) -> FetchOpts {
        FetchOpts {
            min_interval_ms: Some(self.min_interval_ms.unwrap_or(min_interval_ms)),
            backoff_ms: self.backoff_ms,
            ..Default::default()
        }
    }

    /// Every host pointed at one local server, with the pacing out of the way.
    #[cfg(test)]
    pub fn at(base: &str) -> Self {
        Endpoints {
            tvfy: format!("{base}/tvfy"),
            handbook: format!("{base}/handbook"),
            parlwork: format!("{base}/parlwork"),
            wikidata_sparql: format!("{base}/sparql"),
            commons_api: format!("{base}/commons"),
            commons_files: format!("{base}/commons-files"),
            aec_results: format!("{base}/aec-results"),
            aec_profiles: format!("{base}/aec"),
            legislation: format!("{base}/legislation"),
            data_gov: format!("{base}/data-gov"),
            aec_transparency: format!("{base}/aec-transparency"),
            abs_geo: format!("{base}/abs-geo"),
            vic_parliament: format!("{base}/vic"),
            qld_documents: format!("{base}/qld-documents"),
            tas_parliament: format!("{base}/tas"),
            act_parliament: format!("{base}/act"),
            sa_contacts: format!("{base}/sa-contacts"),
            sa_members: format!("{base}/sa-members"),
            vic_legislation: format!("{base}/vic-legislation"),
            act_legislation: format!("{base}/act-legislation"),
            nt_legislation: format!("{base}/nt-legislation"),
            qld_legislation: format!("{base}/qld-legislation"),
            sa_salt: format!("{base}/sa-salt"),
            min_interval_ms: Some(1),
            backoff_ms: Some(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sources_own_spacing_stands_unless_the_config_overrides_it() {
        let production = Endpoints::default();
        assert_eq!(production.opts(1200).min_interval_ms, Some(1200));
        assert_eq!(production.opts(400).min_interval_ms, Some(400));
        assert_eq!(production.opts(400).backoff_ms, None);
        assert!(production.tvfy.starts_with("https://"));

        let test = Endpoints::at("http://127.0.0.1:1");
        assert_eq!(test.opts(1200).min_interval_ms, Some(1));
        assert_eq!(test.opts(400).backoff_ms, Some(1));
    }
}
