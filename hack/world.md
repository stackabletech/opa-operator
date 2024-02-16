# Knab

Knab is a bank with a number of data science teams covering various aspects of the banks operation and compliance obligations.

Usernames are `$firstname.$lastname`.

## Teams

Two of the data science teams are outlined below:

### Compliance and Regulation Analytics

This team sits under the wider Compliance and Regulation team,and is tasked with making use of the banks customer, credit, securities data and produce internal and regulatory reports to aid in regulatory compliance.

Members of the team:

- William A. Lewis (Team Lead)
- Sophia B. Clarke
- Daniel C. King

### Customer Analytics

This team falls under the Customer Service division and is tasked with making use of the banks data for things like:

- Understanding customer call frequency and wait times
- Trends with the usage of physical branches, internet banking and the banking app

They produce monthly and quarterly reports, but also build dashboards to show live data (eg: call queue, and wait time statistics).

Members of the team:

- Pamela D. Scott (Team Lead)
- Justin E. Martin
- Isla F. Williams (contractor with specific user based permissions)

#### Tasks

- **Isla**: Some of the telephony data is not available through APIs until an upgrade is complete. In the meantime, Isla has been brought in as a contractor to help bridge the gap between the data in the aging telephone system and the analytics processes.
  1. Isla manually exports production telephony data from the telephone system web UI. 
  2. Isla runs the data through scripts (dropping PII data) which produce Parquet formatted file (call duration, time between call center staff calls, direct calls vs hunt-group calls).
  3. Isla then loads the files into the **Nonprod** HDFS under a specific path (`hdfs://customer-analytics/telephony/contact-center/*`) that she has access to. _This process will later be automated in production once the telephone system is upgraded._

## Marketing

The Marketing department has access to various dashboards that are maintained by other teams. In this scenario, one of the marketing users has read access to the Customer Analytics dashboard(s) and charts.

Relevant members of the team:

- Mark G. Ketting (needs read access to customer alytics dashboards)



---

![world](./world.drawio.svg)
