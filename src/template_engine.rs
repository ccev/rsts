use tera::Tera;
use std::sync::Mutex;
use anyhow::Result;
use std::path::PathBuf;

pub struct TemplateEngine {
    tera: Mutex<Tera>,
    #[allow(dead_code)]
    base_path: PathBuf,
}

impl TemplateEngine {

    pub fn new(base_path: PathBuf) -> Result<Self> {
        // Ensure the directory exists
        if !base_path.exists() {
            std::fs::create_dir_all(&base_path)?;
        }

        let pattern = format!("{}/**/*.json", base_path.to_string_lossy());
        let mut tera = match Tera::new(&pattern) {
            Ok(t) => t,
            Err(e) => {
                // If it's just "no templates found", initialize empty
                if e.to_string().contains("no templates found") || e.to_string().contains("match any files") {
                    Tera::default()
                } else {
                    return Err(e.into());
                }
            }
        };

        tera.autoescape_on(vec![]); 

        Ok(Self {
            tera: Mutex::new(tera),
            base_path,
        })
    }



    pub fn render(&self, template_name: &str, context: &tera::Context) -> Result<String> {

        let tera = self.tera.lock().unwrap();

        

        let template_file = format!("{}.json", template_name);

        tera.render(&template_file, context).map_err(|e| {

            let loaded = tera.get_template_names().collect::<Vec<_>>();

            anyhow::anyhow!("Tera Render Error: {}. Requested: '{}'. Loaded: {:?}", e, template_file, loaded)

        })

    }

}
